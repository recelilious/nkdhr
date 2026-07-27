//! PipeWire integration for the Audio module.
//!
//! PipeWire's client API is entirely event-driven and its proxy/listener
//! types are `Rc`-based, not `Send` — they must all stay confined to one
//! thread running PipeWire's own main loop. This module spawns that
//! thread and keeps a small, plain-data cache of the default sink's and
//! source's name/volume/mute that [`crate::modules::audio::Audio`] reads
//! synchronously from any thread. Mutating calls (`set_sink_volume`/
//! `set_sink_muted`) go the other way: they queue a command that the
//! worker thread's own timer picks up and applies via `Node::set_param`,
//! since `Node` isn't `Send` and can't be called from outside its thread
//! directly.
//!
//! Device switching while `nkdhrd` is running (e.g. plugging in a USB
//! headset that becomes the new default) is intentionally best-effort
//! here: the cache updates whenever PipeWire tells us about it, but there
//! is no reconnection/retry logic beyond what PipeWire itself provides.
//! Full live-update semantics are CTRL-4's job, not CTRL-2's.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Cursor;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use pipewire::metadata::{Metadata, MetadataListener};
use pipewire::node::{Node, NodeListener};
use pipewire::registry::{GlobalObject, RegistryRc};
use pipewire::spa::param::ParamType;
use pipewire::spa::pod::deserialize::PodDeserializer;
use pipewire::spa::pod::serialize::PodSerializer;
use pipewire::spa::pod::{Object, Pod, Property, PropertyFlags, Value, ValueArray};
use pipewire::spa::utils::SpaTypes;
use pipewire::spa::utils::dict::DictRef;
use pipewire::types::ObjectType;

/// How often the worker thread checks for pending [`AudioCommand`]s.
/// PipeWire's client API has no cross-thread wakeup primitive simple
/// enough to justify over this for a volume-change command, which has no
/// latency requirement stricter than "feels instant to a human".
const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The default sink's and source's last known volume/mute, as read by
/// [`crate::modules::audio::Audio::get_status`]. Every field is `None`
/// until the worker thread has resolved and read the current default
/// device at least once, or if there is no such device at all.
#[derive(Default, Clone)]
pub struct AudioState {
    pub sink_name: Option<String>,
    pub sink_volume_percent: Option<u8>,
    pub sink_muted: Option<bool>,
    pub source_name: Option<String>,
    pub source_volume_percent: Option<u8>,
    pub source_muted: Option<bool>,
}

pub type SharedAudioState = Arc<Mutex<AudioState>>;

/// A mutating request for the current default sink. Only the sink
/// (playback) is mutable for CTRL-3; the source (microphone) is
/// read-only, matching nkdhrctl's own `set-volume`/`mute` scope.
enum AudioCommand {
    SetSinkVolume(u8),
    SetSinkMuted(bool),
}

/// Handle returned by [`spawn`]: the live status cache plus a way to
/// queue mutating requests for the worker thread to apply.
pub struct PipeWireHandle {
    pub state: SharedAudioState,
    commands: Sender<AudioCommand>,
}

impl PipeWireHandle {
    pub fn set_sink_volume(&self, percent: u8) {
        let _ = self.commands.send(AudioCommand::SetSinkVolume(percent));
    }

    pub fn set_sink_muted(&self, muted: bool) {
        let _ = self.commands.send(AudioCommand::SetSinkMuted(muted));
    }
}

/// Spawns the PipeWire worker thread and returns the handle it keeps
/// updated, and takes commands from, for the lifetime of the process.
pub fn spawn() -> PipeWireHandle {
    let state: SharedAudioState = Arc::new(Mutex::new(AudioState::default()));
    let state_for_thread = Arc::clone(&state);
    let (commands, command_rx) = mpsc::channel();

    thread::spawn(move || {
        if let Err(err) = run(state_for_thread, command_rx) {
            eprintln!("nkdhrd: PipeWire worker exited: {err}");
        }
    });

    PipeWireHandle { state, commands }
}

/// Which default audio device a tracked node might be — playback (sink)
/// or capture (source). Drives both the PipeWire-side filtering (media
/// class) and the "default" metadata key nkdhr watches to learn which
/// node is currently the default.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DeviceKind {
    Sink,
    Source,
}

impl DeviceKind {
    const ALL: [DeviceKind; 2] = [DeviceKind::Sink, DeviceKind::Source];

    fn media_class(self) -> &'static str {
        match self {
            DeviceKind::Sink => "Audio/Sink",
            DeviceKind::Source => "Audio/Source",
        }
    }

    fn metadata_key(self) -> &'static str {
        match self {
            DeviceKind::Sink => "default.audio.sink",
            DeviceKind::Source => "default.audio.source",
        }
    }

    fn from_media_class(media_class: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.media_class() == media_class)
    }

    fn from_metadata_key(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.metadata_key() == key)
    }
}

/// A node's cached name and last-read `Props`, keyed by its registry
/// global id. Kept alongside the live `Node`/`NodeListener` so the
/// subscription stays alive for as long as the node exists.
struct BoundNode {
    name: String,
    volume_percent: Option<u8>,
    /// Number of channels last seen in `channelVolumes`, so a volume
    /// change can be replicated across all of them instead of collapsing
    /// e.g. a stereo device down to one channel. Defaults to a plain
    /// stereo assumption until a real `Props` update has been read.
    channels: usize,
    muted: Option<bool>,
    node: Node,
    _listener: NodeListener,
}

/// Everything tracked for one [`DeviceKind`]: which node name is currently
/// the default (per the "default" metadata object) and every known node
/// of that kind, keyed by registry global id.
#[derive(Default)]
struct KindState {
    target_name: Option<String>,
    nodes: HashMap<u32, BoundNode>,
}

impl KindState {
    fn resolved(&self) -> Option<&BoundNode> {
        let target = self.target_name.as_ref()?;
        self.nodes.values().find(|node| &node.name == target)
    }

    fn resolved_mut(&mut self) -> Option<&mut BoundNode> {
        let target = self.target_name.clone()?;
        self.nodes.values_mut().find(|node| node.name == target)
    }
}

struct Tracker {
    state: SharedAudioState,
    registry: RegistryRc,
    sinks: KindState,
    sources: KindState,
    _metadata: Option<(Metadata, MetadataListener)>,
}

impl Tracker {
    fn kind_state_mut(&mut self, kind: DeviceKind) -> &mut KindState {
        match kind {
            DeviceKind::Sink => &mut self.sinks,
            DeviceKind::Source => &mut self.sources,
        }
    }

    fn reconcile(&mut self) {
        let sink = self.sinks.resolved();
        let source = self.sources.resolved();

        let mut state = self.state.lock().expect("audio state mutex poisoned");
        if let Some(node) = sink {
            state.sink_name = Some(node.name.clone());
            state.sink_volume_percent = node.volume_percent;
            state.sink_muted = node.muted;
        }
        if let Some(node) = source {
            state.source_name = Some(node.name.clone());
            state.source_volume_percent = node.volume_percent;
            state.source_muted = node.muted;
        }
    }

    fn apply(&mut self, command: AudioCommand) {
        let Some(sink) = self.sinks.resolved_mut() else {
            return;
        };

        let bytes = match command {
            AudioCommand::SetSinkVolume(percent) => props_pod(Some((percent, sink.channels)), None),
            AudioCommand::SetSinkMuted(muted) => props_pod(None, Some(muted)),
        };
        let Some(pod) = Pod::from_bytes(&bytes) else {
            return;
        };
        sink.node.set_param(ParamType::Props, 0, pod);
    }
}

fn run(state: SharedAudioState, commands: Receiver<AudioCommand>) -> Result<(), pipewire::Error> {
    pipewire::init();

    let main_loop = pipewire::main_loop::MainLoopRc::new(None)?;
    let context = pipewire::context::ContextRc::new(&main_loop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;

    let tracker = Rc::new(RefCell::new(Tracker {
        state,
        registry: registry.clone(),
        sinks: KindState::default(),
        sources: KindState::default(),
        _metadata: None,
    }));

    let tracker_for_global = Rc::clone(&tracker);
    let tracker_for_remove = Rc::clone(&tracker);
    let _registry_listener = registry
        .add_listener_local()
        .global(move |obj| handle_global(&tracker_for_global, obj))
        .global_remove(move |id| {
            let mut tracker = tracker_for_remove.borrow_mut();
            let removed = tracker.sinks.nodes.remove(&id).is_some()
                || tracker.sources.nodes.remove(&id).is_some();
            if removed {
                tracker.reconcile();
            }
        })
        .register();

    let tracker_for_commands = Rc::clone(&tracker);
    let timer = main_loop.loop_().add_timer(move |_expirations| {
        for command in commands.try_iter() {
            tracker_for_commands.borrow_mut().apply(command);
        }
    });
    timer.update_timer(Some(COMMAND_POLL_INTERVAL), Some(COMMAND_POLL_INTERVAL));

    main_loop.run();
    Ok(())
}

fn handle_global(tracker: &Rc<RefCell<Tracker>>, obj: &GlobalObject<&DictRef>) {
    match obj.type_ {
        ObjectType::Node => handle_node_global(tracker, obj),
        ObjectType::Metadata => handle_metadata_global(tracker, obj),
        _ => {}
    }
}

fn handle_node_global(tracker: &Rc<RefCell<Tracker>>, obj: &GlobalObject<&DictRef>) {
    let Some(kind) = obj
        .props
        .and_then(|props| props.get(&pipewire::keys::MEDIA_CLASS))
        .and_then(DeviceKind::from_media_class)
    else {
        return;
    };
    let Some(name) = obj
        .props
        .and_then(|props| props.get(&pipewire::keys::NODE_NAME))
    else {
        return;
    };
    let name = name.to_owned();

    let Ok(node): Result<Node, _> = tracker.borrow().registry.bind(obj) else {
        return;
    };

    let id = obj.id;
    let tracker_for_param = Rc::clone(tracker);
    let listener = node
        .add_listener_local()
        .param(move |_seq, param_id, _index, _next, param| {
            if param_id != ParamType::Props {
                return;
            }
            let Some(param) = param else {
                return;
            };
            let (volume_percent, channels, muted) = parse_props(param);
            let mut tracker = tracker_for_param.borrow_mut();
            if let Some(node) = tracker.kind_state_mut(kind).nodes.get_mut(&id) {
                node.volume_percent = volume_percent.or(node.volume_percent);
                node.channels = channels.unwrap_or(node.channels);
                node.muted = muted.or(node.muted);
            }
            tracker.reconcile();
        })
        .register();

    node.subscribe_params(&[ParamType::Props]);
    node.enum_params(0, Some(ParamType::Props), 0, u32::MAX);

    let mut tracker = tracker.borrow_mut();
    tracker.kind_state_mut(kind).nodes.insert(
        id,
        BoundNode {
            name,
            volume_percent: None,
            channels: 2,
            muted: None,
            node,
            _listener: listener,
        },
    );
    tracker.reconcile();
}

fn handle_metadata_global(tracker: &Rc<RefCell<Tracker>>, obj: &GlobalObject<&DictRef>) {
    let is_default_metadata =
        obj.props.and_then(|props| props.get("metadata.name")) == Some("default");
    if !is_default_metadata {
        return;
    }

    let Ok(metadata): Result<Metadata, _> = tracker.borrow().registry.bind(obj) else {
        return;
    };

    let tracker_for_property = Rc::clone(tracker);
    let listener = metadata
        .add_listener_local()
        .property(move |_subject, key, _type, value| {
            let Some(kind) = key.and_then(DeviceKind::from_metadata_key) else {
                return 0;
            };
            let target = value.and_then(default_node_name_from_json);
            let mut tracker = tracker_for_property.borrow_mut();
            tracker.kind_state_mut(kind).target_name = target;
            tracker.reconcile();
            0
        })
        .register();

    tracker.borrow_mut()._metadata = Some((metadata, listener));
}

/// PipeWire's "default" metadata encodes `default.audio.sink`/
/// `default.audio.source` as a JSON object, e.g.
/// `{"name":"alsa_output.pci-0000_00_1f.3.analog-stereo"}`, rather than as
/// a plain string.
fn default_node_name_from_json(value: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(value).ok()?;
    value.get("name")?.as_str().map(str::to_owned)
}

/// Reads `channelVolumes` (averaged, as a 0-100 percentage, alongside the
/// channel count) and `mute` out of a `Props` param pod.
fn parse_props(pod: &Pod) -> (Option<u8>, Option<usize>, Option<bool>) {
    let Ok((_, Value::Object(object))) = PodDeserializer::deserialize_any_from(pod.as_bytes())
    else {
        return (None, None, None);
    };

    let mut volume_percent = None;
    let mut channels = None;
    let mut muted = None;
    for property in object.properties {
        match (property.key, property.value) {
            (key, Value::ValueArray(ValueArray::Float(values)))
                if key == pipewire::spa::sys::SPA_PROP_channelVolumes && !values.is_empty() =>
            {
                let average = values.iter().sum::<f32>() / values.len() as f32;
                volume_percent = Some((average * 100.0).round().clamp(0.0, 100.0) as u8);
                channels = Some(values.len());
            }
            (key, Value::Bool(value)) if key == pipewire::spa::sys::SPA_PROP_mute => {
                muted = Some(value);
            }
            _ => {}
        }
    }

    (volume_percent, channels, muted)
}

/// Builds a `Props` param pod setting `channelVolumes` (replicated across
/// `channels`) and/or `mute`. At least one of `volume`/`muted` should be
/// `Some`; an empty pod is valid but pointless.
fn props_pod(volume: Option<(u8, usize)>, muted: Option<bool>) -> Vec<u8> {
    let mut properties = Vec::new();
    if let Some((percent, channels)) = volume {
        let linear = f32::from(percent.min(100)) / 100.0;
        properties.push(Property {
            key: pipewire::spa::sys::SPA_PROP_channelVolumes,
            flags: PropertyFlags::empty(),
            value: Value::ValueArray(ValueArray::Float(vec![linear; channels.max(1)])),
        });
    }
    if let Some(muted) = muted {
        properties.push(Property {
            key: pipewire::spa::sys::SPA_PROP_mute,
            flags: PropertyFlags::empty(),
            value: Value::Bool(muted),
        });
    }

    let value = Value::Object(Object {
        type_: SpaTypes::ObjectParamProps.as_raw(),
        id: ParamType::Props.as_raw(),
        properties,
    });

    let (cursor, _size) = PodSerializer::serialize(Cursor::new(Vec::new()), &value)
        .expect("a Props object with only channelVolumes/mute always serializes");
    cursor.into_inner()
}
