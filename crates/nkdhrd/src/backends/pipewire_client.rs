//! PipeWire integration for the Audio module.
//!
//! PipeWire's client API is entirely event-driven and its proxy/listener
//! types are `Rc`-based, not `Send` — they must all stay confined to one
//! thread running PipeWire's own main loop. This module spawns that
//! thread and keeps a small, plain-data cache of the default sink's and
//! source's name/volume/mute that [`crate::modules::audio::Audio`] reads
//! synchronously from any thread.
//!
//! Device switching while `nkdhrd` is running (e.g. plugging in a USB
//! headset that becomes the new default) is intentionally best-effort
//! here: the cache updates whenever PipeWire tells us about it, but there
//! is no reconnection/retry logic beyond what PipeWire itself provides.
//! Full live-update semantics are CTRL-4's job, not CTRL-2's.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::thread;

use pipewire::metadata::{Metadata, MetadataListener};
use pipewire::node::{Node, NodeListener};
use pipewire::registry::{GlobalObject, RegistryRc};
use pipewire::spa::param::ParamType;
use pipewire::spa::pod::Pod;
use pipewire::spa::pod::deserialize::PodDeserializer;
use pipewire::spa::utils::dict::DictRef;
use pipewire::types::ObjectType;

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

/// Spawns the PipeWire worker thread and returns the shared state handle
/// it keeps updated for the lifetime of the process.
pub fn spawn() -> SharedAudioState {
    let state: SharedAudioState = Arc::new(Mutex::new(AudioState::default()));
    let state_for_thread = Arc::clone(&state);

    thread::spawn(move || {
        if let Err(err) = run(state_for_thread) {
            eprintln!("nkdhrd: PipeWire worker exited: {err}");
        }
    });

    state
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
    muted: Option<bool>,
    _node: Node,
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
}

fn run(state: SharedAudioState) -> Result<(), pipewire::Error> {
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
    let _registry_listener = registry
        .add_listener_local()
        .global(move |obj| handle_global(&tracker_for_global, obj))
        .global_remove(move |id| {
            let mut tracker = tracker.borrow_mut();
            let removed = tracker.sinks.nodes.remove(&id).is_some()
                || tracker.sources.nodes.remove(&id).is_some();
            if removed {
                tracker.reconcile();
            }
        })
        .register();

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
            let (volume_percent, muted) = parse_props(param);
            let mut tracker = tracker_for_param.borrow_mut();
            if let Some(node) = tracker.kind_state_mut(kind).nodes.get_mut(&id) {
                node.volume_percent = volume_percent.or(node.volume_percent);
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
            muted: None,
            _node: node,
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

/// Reads `channelVolumes` (averaged, as a 0-100 percentage) and `mute`
/// out of a `Props` param pod.
fn parse_props(pod: &Pod) -> (Option<u8>, Option<bool>) {
    let Ok((_, pipewire::spa::pod::Value::Object(object))) =
        PodDeserializer::deserialize_any_from(pod.as_bytes())
    else {
        return (None, None);
    };

    let mut volume_percent = None;
    let mut muted = None;
    for property in object.properties {
        match (property.key, property.value) {
            (
                key,
                pipewire::spa::pod::Value::ValueArray(pipewire::spa::pod::ValueArray::Float(
                    channels,
                )),
            ) if key == pipewire::spa::sys::SPA_PROP_channelVolumes && !channels.is_empty() => {
                let average = channels.iter().sum::<f32>() / channels.len() as f32;
                volume_percent = Some((average * 100.0).round().clamp(0.0, 100.0) as u8);
            }
            (key, pipewire::spa::pod::Value::Bool(value))
                if key == pipewire::spa::sys::SPA_PROP_mute =>
            {
                muted = Some(value);
            }
            _ => {}
        }
    }

    (volume_percent, muted)
}
