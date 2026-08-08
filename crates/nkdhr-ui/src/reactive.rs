//! Root-local reactive values that queue invalidation without re-entering the tree.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::{Rc, Weak},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{Invalidation, WidgetId};

static NEXT_ROOT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy)]
struct QueuedInvalidation {
    widget: WidgetId,
    invalidation: Invalidation,
}

#[derive(Debug)]
pub(crate) struct RootReactivity {
    id: u64,
    alive: RefCell<HashSet<WidgetId>>,
    queued: RefCell<Vec<QueuedInvalidation>>,
}

impl RootReactivity {
    pub fn new() -> Rc<Self> {
        let id = NEXT_ROOT_ID.fetch_add(1, Ordering::Relaxed);
        Rc::new(Self {
            id: id.max(1),
            alive: RefCell::new(HashSet::new()),
            queued: RefCell::new(Vec::new()),
        })
    }

    pub fn insert(&self, widget: WidgetId) {
        self.alive.borrow_mut().insert(widget);
    }

    pub fn remove(&self, widget: WidgetId) {
        self.alive.borrow_mut().remove(&widget);
    }

    pub fn is_alive(&self, widget: WidgetId) -> bool {
        self.alive.borrow().contains(&widget)
    }

    pub fn drain(&self) -> Vec<(WidgetId, Invalidation)> {
        self.queued
            .borrow_mut()
            .drain(..)
            .map(|queued| (queued.widget, queued.invalidation))
            .collect()
    }

    fn enqueue(&self, widget: WidgetId, invalidation: Invalidation) {
        self.queued.borrow_mut().push(QueuedInvalidation {
            widget,
            invalidation,
        });
    }
}

#[derive(Debug)]
struct Subscription {
    root: Weak<RootReactivity>,
    widget: WidgetId,
    invalidation: Invalidation,
}

#[derive(Debug)]
struct ReactiveInner<T> {
    value: RefCell<T>,
    subscriptions: RefCell<HashMap<(u64, WidgetId), Subscription>>,
}

trait SubscriptionSource {
    fn unsubscribe(&self, root: u64, widget: WidgetId);
}

impl<T> SubscriptionSource for ReactiveInner<T> {
    fn unsubscribe(&self, root: u64, widget: WidgetId) {
        self.subscriptions.borrow_mut().remove(&(root, widget));
    }
}

pub(crate) struct SubscriptionToken {
    source: Weak<dyn SubscriptionSource>,
    root: u64,
    widget: WidgetId,
}

impl SubscriptionToken {
    fn matches(&self, source: &Weak<dyn SubscriptionSource>, root: u64, widget: WidgetId) -> bool {
        self.source.ptr_eq(source) && self.root == root && self.widget == widget
    }
}

impl Drop for SubscriptionToken {
    fn drop(&mut self) {
        if let Some(source) = self.source.upgrade() {
            source.unsubscribe(self.root, self.widget);
        }
    }
}

/// A cloneable UI-thread value. Mutations only enqueue invalidation; widget
/// callbacks run later when the owning root reaches an explicit boundary.
#[derive(Debug)]
pub struct Reactive<T> {
    inner: Rc<ReactiveInner<T>>,
}

impl<T> Clone for Reactive<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Rc::clone(&self.inner),
        }
    }
}

impl<T> Reactive<T> {
    pub fn new(value: T) -> Self {
        Self {
            inner: Rc::new(ReactiveInner {
                value: RefCell::new(value),
                subscriptions: RefCell::new(HashMap::new()),
            }),
        }
    }

    pub fn set(&self, value: T) {
        *self.inner.value.borrow_mut() = value;
        self.notify();
    }

    pub fn update(&self, update: impl FnOnce(&mut T)) {
        update(&mut self.inner.value.borrow_mut());
        self.notify();
    }

    pub fn get(&self) -> T
    where
        T: Clone,
    {
        self.inner.value.borrow().clone()
    }

    pub(crate) fn watch(
        &self,
        root: &Rc<RootReactivity>,
        widget: WidgetId,
        invalidation: Invalidation,
        tokens: &mut Vec<SubscriptionToken>,
    ) -> T
    where
        T: Clone + 'static,
    {
        let key = (root.id, widget);
        let mut subscriptions = self.inner.subscriptions.borrow_mut();
        subscriptions
            .entry(key)
            .and_modify(|subscription| subscription.invalidation |= invalidation)
            .or_insert_with(|| Subscription {
                root: Rc::downgrade(root),
                widget,
                invalidation,
            });
        drop(subscriptions);
        let source: Rc<dyn SubscriptionSource> = self.inner.clone();
        let source = Rc::downgrade(&source);
        if !tokens
            .iter()
            .any(|token| token.matches(&source, root.id, widget))
        {
            tokens.push(SubscriptionToken {
                source,
                root: root.id,
                widget,
            });
        }
        self.get()
    }

    fn notify(&self) {
        self.inner
            .subscriptions
            .borrow_mut()
            .retain(|_, subscription| {
                let Some(root) = subscription.root.upgrade() else {
                    return false;
                };
                if !root.is_alive(subscription.widget) {
                    return false;
                }
                root.enqueue(subscription.widget, subscription.invalidation);
                true
            });
    }
}

impl<T: PartialEq> Reactive<T> {
    pub fn set_if_changed(&self, value: T) -> bool {
        if *self.inner.value.borrow() == value {
            return false;
        }
        self.set(value);
        true
    }
}
