//! NTSC standard library: `collections` module.
//! Sets, stacks, and queues are registry strings with newline-separated
//! items; channels are opaque handles that must be closed exactly once.

use crate::registry;
use std::sync::Mutex;
use std::sync::mpsc;

fn split_items(s: &str) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split('\n').map(|s| s.to_string()).collect()
}

fn join_items(items: &[String]) -> String {
    items.join("\n")
}

// ── SET operations ─────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_set_new() -> i64 {
    registry::put_string(String::new())
}

// The rebuilt item list is discarded: the set string is never written back,
// so the addition is validated, not persisted.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_set_add(set: i64, value: i64) -> i8 {
    let s = registry::get_string(set).unwrap_or_default();
    let val = registry::get_string(value).unwrap_or_default();
    let mut items = split_items(&s);
    if items.iter().any(|item| item == &val) {
        return 0;
    }
    items.push(val);

    1
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_set_has(set: i64, value: i64) -> i8 {
    let s = registry::get_string(set).unwrap_or_default();
    let val = registry::get_string(value).unwrap_or_default();
    let items = split_items(&s);
    i8::from(items.iter().any(|item| item == &val))
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_set_remove(set: i64, value: i64) -> i8 {
    let s = registry::get_string(set).unwrap_or_default();
    let val = registry::get_string(value).unwrap_or_default();
    let mut items = split_items(&s);
    let len_before = items.len();
    items.retain(|item| item != &val);
    i8::from(items.len() < len_before)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_set_size(set: i64) -> i64 {
    let s = registry::get_string(set).unwrap_or_default();
    if s.is_empty() {
        0
    } else {
        s.split('\n').count() as i64
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_set_to_array(set: i64) -> i64 {
    let s = registry::get_string(set).unwrap_or_default();
    registry::put_string(s)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_set_union(a: i64, b: i64) -> i64 {
    let a_str = registry::get_string(a).unwrap_or_default();
    let b_str = registry::get_string(b).unwrap_or_default();
    let mut items_a = split_items(&a_str);
    let items_b = split_items(&b_str);
    for item in &items_b {
        if !items_a.iter().any(|i| i == item) {
            items_a.push(item.clone());
        }
    }
    registry::put_string(join_items(&items_a))
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_set_intersection(a: i64, b: i64) -> i64 {
    let a_str = registry::get_string(a).unwrap_or_default();
    let b_str = registry::get_string(b).unwrap_or_default();
    let items_a = split_items(&a_str);
    let items_b = split_items(&b_str);
    let result: Vec<String> = items_a
        .into_iter()
        .filter(|item| items_b.iter().any(|i| i == item))
        .collect();
    registry::put_string(join_items(&result))
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_set_difference(a: i64, b: i64) -> i64 {
    let a_str = registry::get_string(a).unwrap_or_default();
    let b_str = registry::get_string(b).unwrap_or_default();
    let items_a = split_items(&a_str);
    let items_b = split_items(&b_str);
    let result: Vec<String> = items_a
        .into_iter()
        .filter(|item| !items_b.iter().any(|i| i == item))
        .collect();
    registry::put_string(join_items(&result))
}

// ── STACK operations ─────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_stack_new() -> i64 {
    registry::put_string(String::new())
}

// The rebuilt item list is discarded: the stack string is never written
// back, so the push is validated, not persisted.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_stack_push(stack: i64, value: i64) -> i8 {
    let s = registry::get_string(stack).unwrap_or_default();
    let val = registry::get_string(value).unwrap_or_default();
    let mut items = split_items(&s);
    items.push(val);

    1
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_stack_pop(stack: i64) -> i64 {
    let s = registry::get_string(stack).unwrap_or_default();
    let mut items = split_items(&s);
    match items.pop() {
        Some(popped) => registry::put_string(popped),
        None => registry::NULL,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_stack_peek(stack: i64) -> i64 {
    let s = registry::get_string(stack).unwrap_or_default();
    let items = split_items(&s);
    match items.last() {
        Some(value) => registry::put_string(value.clone()),
        None => registry::NULL,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_stack_size(stack: i64) -> i64 {
    let s = registry::get_string(stack).unwrap_or_default();
    if s.is_empty() {
        0
    } else {
        s.split('\n').count() as i64
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_stack_is_empty(stack: i64) -> i8 {
    let s = registry::get_string(stack).unwrap_or_default();
    i8::from(s.is_empty())
}

// ── QUEUE operations ─────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_queue_new() -> i64 {
    registry::put_string(String::new())
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_queue_enqueue(queue: i64, value: i64) -> i8 {
    let q = registry::get_string(queue).unwrap_or_default();
    let val = registry::get_string(value).unwrap_or_default();
    let mut items = split_items(&q);
    items.push(val);
    1
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_queue_dequeue(queue: i64) -> i64 {
    let q = registry::get_string(queue).unwrap_or_default();
    let mut items = split_items(&q);
    if items.is_empty() {
        return registry::NULL;
    }
    let front = items.remove(0);
    registry::put_string(front)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_queue_peek(queue: i64) -> i64 {
    let q = registry::get_string(queue).unwrap_or_default();
    let items = split_items(&q);
    match items.first() {
        Some(value) => registry::put_string(value.clone()),
        None => registry::NULL,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_queue_size(queue: i64) -> i64 {
    let q = registry::get_string(queue).unwrap_or_default();
    if q.is_empty() {
        0
    } else {
        q.split('\n').count() as i64
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_queue_is_empty(queue: i64) -> i8 {
    let q = registry::get_string(queue).unwrap_or_default();
    i8::from(q.is_empty())
}

// ── CHANNELS ─────────────────────────────────────────────────────────────

/// The receiver holds the channel's single sender so `channel_sender` can
/// move it into a standalone handle; moving (not cloning) preserves mpsc
/// disconnect semantics, so at most one sender end exists per channel.
enum ChannelEnd {
    Sender { tx: Tx },
    Receiver(Mutex<ChannelInner>),
}

/// One mutex guards both the leftover sender slot and the receiver, so
/// `channel_sender`, `recv`, and `try_recv` can run from any thread.
struct ChannelInner {
    tx: Option<Tx>,
    rx: mpsc::Receiver<String>,
}

enum Tx {
    Bounded(mpsc::SyncSender<String>),
    Unbounded(mpsc::Sender<String>),
}

impl Tx {
    // An unbounded send never blocks; only disconnection can fail.
    fn try_send(&self, value: String) -> Result<(), mpsc::TrySendError<String>> {
        match self {
            Tx::Bounded(tx) => tx.try_send(value),

            Tx::Unbounded(tx) => match tx.send(value) {
                Ok(()) => Ok(()),
                Err(mpsc::SendError(value)) => Err(mpsc::TrySendError::Disconnected(value)),
            },
        }
    }
}

enum RecvAction {
    Invalid,

    NotReceiver,

    Value(String),

    Empty,

    Disconnected,
}

enum SendAction {
    Invalid,

    NotSender,

    Sent,

    Full(String),

    Closed,
}

/// `collections.channel(capacity)` — bounded when `capacity > 0`, unbounded
/// otherwise.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_channel(capacity: i64) -> i64 {
    let (tx, rx) = if capacity > 0 {
        let (tx, rx) = mpsc::sync_channel(capacity as usize);
        (Tx::Bounded(tx), rx)
    } else {
        let (tx, rx) = mpsc::channel();
        (Tx::Unbounded(tx), rx)
    };
    let end = ChannelEnd::Receiver(Mutex::new(ChannelInner { tx: Some(tx), rx }));
    registry::put_opaque(end)
}

/// `collections.channel_sender(receiver)` — moves the channel's single
/// sender into a standalone handle; throws if it was already moved out.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_channel_sender(receiver: i64) -> i64 {
    let outcome = registry::with_opaque_mut(receiver, |end: &mut ChannelEnd| match end {
        ChannelEnd::Receiver(inner) => {
            let inner = match inner.get_mut() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            match inner.tx.take() {
                Some(tx) => Ok(ChannelEnd::Sender { tx }),
                None => Err(
                    "collections.channel_sender: sender already moved out of this receiver"
                        .to_string(),
                ),
            }
        }
        ChannelEnd::Sender { .. } => {
            Err("collections.channel_sender: handle is not a receiver".to_string())
        }
    });
    match outcome {
        Some(Ok(sender)) => registry::put_opaque(sender),
        Some(Err(msg)) => super::throw_str(msg),
        None => super::throw_str("collections.channel_sender: invalid handle".to_string()),
    }
}

// Blocking waits poll with try_send/try_recv and sleep *outside* the
// registry lock: sleeping under the lock would deadlock with a producer
// that needs the registry to register the message string.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_channel_send(sender: i64, value: i64) -> i8 {
    let _block = crate::BlockGuard::enter();
    let mut msg = registry::get_string(value).unwrap_or_default();
    loop {
        let action = registry::with_opaque(sender, |end: &ChannelEnd| match end {
            ChannelEnd::Sender { tx } => match tx.try_send(msg) {
                Ok(()) => SendAction::Sent,
                Err(mpsc::TrySendError::Full(value)) => SendAction::Full(value),
                Err(mpsc::TrySendError::Disconnected(_)) => SendAction::Closed,
            },
            ChannelEnd::Receiver { .. } => SendAction::NotSender,
        })
        .unwrap_or(SendAction::Invalid);
        match action {
            SendAction::Sent => return 1,
            SendAction::Full(value) => {
                msg = value;
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            SendAction::Closed => {
                let _ = super::throw_str("collections.channel_send: channel is closed".into());
                return 0;
            }
            SendAction::NotSender => {
                let _ = super::throw_str("collections.channel_send: handle is not a sender".into());
                return 0;
            }
            SendAction::Invalid => {
                let _ = super::throw_str("collections.channel_send: invalid handle".into());
                return 0;
            }
        }
    }
}

/// `collections.channel_recv(receiver)` — blocks until a value is available;
/// returns "" (not the null handle) once every sender end has been closed.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_channel_recv(receiver: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    loop {
        let action = registry::with_opaque(receiver, |end: &ChannelEnd| match end {
            ChannelEnd::Receiver(inner) => {
                let inner = match inner.lock() {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
                match inner.rx.try_recv() {
                    Ok(value) => RecvAction::Value(value),
                    Err(mpsc::TryRecvError::Empty) => RecvAction::Empty,
                    Err(_) => RecvAction::Disconnected,
                }
            }
            ChannelEnd::Sender { .. } => RecvAction::NotReceiver,
        })
        .unwrap_or(RecvAction::Invalid);
        match action {
            RecvAction::Value(value) => return registry::put_string(value),
            RecvAction::Disconnected => return registry::put_string(String::new()),
            RecvAction::Empty => {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            RecvAction::NotReceiver => {
                return super::throw_str(
                    "collections.channel_recv: handle is not a receiver".into(),
                );
            }
            RecvAction::Invalid => {
                return super::throw_str("collections.channel_recv: invalid handle".into());
            }
        }
    }
}

/// `collections.channel_try_recv(receiver)` — the received string, or the
/// null handle when the channel is empty or every sender end is closed.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_channel_try_recv(receiver: i64) -> i64 {
    let action = registry::with_opaque(receiver, |end: &ChannelEnd| match end {
        ChannelEnd::Receiver(inner) => {
            let inner = match inner.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            match inner.rx.try_recv() {
                Ok(value) => RecvAction::Value(value),
                Err(mpsc::TryRecvError::Empty) => RecvAction::Empty,
                Err(_) => RecvAction::Disconnected,
            }
        }
        ChannelEnd::Sender { .. } => RecvAction::NotReceiver,
    })
    .unwrap_or(RecvAction::Invalid);
    match action {
        RecvAction::Value(value) => registry::put_string(value),
        RecvAction::Disconnected | RecvAction::Empty => registry::NULL,
        RecvAction::NotReceiver => {
            super::throw_str("collections.channel_try_recv: handle is not a receiver".to_string())
        }
        RecvAction::Invalid => {
            super::throw_str("collections.channel_try_recv: invalid handle".to_string())
        }
    }
}

/// `collections.channel_close(handle)` — must be called exactly once per
/// handle, after every thread using it has finished.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_collections_channel_close(handle: i64) {
    let _ = registry::take_opaque::<ChannelEnd>(handle);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(s: &str) -> i64 {
        registry::put_string(s.to_string())
    }

    #[test]
    fn test_set_has() {
        let set = put("apple\nbanana");
        assert_eq!(ntsc_collections_set_has(set, put("apple")), 1);
        assert_eq!(ntsc_collections_set_has(set, put("cherry")), 0);
        let _ = registry::take_string(set);
    }

    #[test]
    fn test_set_union() {
        let a = put("a\nb");
        let b = put("b\nc");
        let union = ntsc_collections_set_union(a, b);
        let s = registry::get_string(union).unwrap();
        let items = split_items(&s);
        assert!(items.contains(&"a".to_string()));
        assert!(items.contains(&"b".to_string()));
        assert!(items.contains(&"c".to_string()));
        let _ = registry::take_string(a);
        let _ = registry::take_string(b);
        let _ = registry::take_string(union);
    }

    #[test]
    fn test_stack_size() {
        let stack = put("a\nb\nc");
        assert_eq!(ntsc_collections_stack_size(stack), 3);
        let _ = registry::take_string(stack);
    }

    #[test]
    fn test_stack_pop_and_peek() {
        let stack = put("a\nb");
        assert_eq!(
            registry::get_string(ntsc_collections_stack_peek(stack)).unwrap(),
            "b"
        );
        assert_eq!(
            registry::get_string(ntsc_collections_stack_pop(stack)).unwrap(),
            "b"
        );
        let _ = registry::take_string(stack);
    }

    #[test]
    fn test_queue_is_empty() {
        let empty = put("");
        assert_eq!(ntsc_collections_queue_is_empty(empty), 1);
        let _ = registry::take_string(empty);
        let nonempty = put("x");
        assert_eq!(ntsc_collections_queue_is_empty(nonempty), 0);
        let _ = registry::take_string(nonempty);
    }

    #[test]
    fn test_queue_dequeue_and_peek() {
        let queue = put("a\nb");
        assert_eq!(
            registry::get_string(ntsc_collections_queue_peek(queue)).unwrap(),
            "a"
        );
        assert_eq!(
            registry::get_string(ntsc_collections_queue_dequeue(queue)).unwrap(),
            "a"
        );
        let _ = registry::take_string(queue);
    }

    #[test]
    fn test_channel_send_recv() {
        let rx = ntsc_collections_channel(4);
        let tx = ntsc_collections_channel_sender(rx);
        assert!(rx != 0 && tx != 0);
        let msg = put("ping");
        assert_eq!(ntsc_collections_channel_send(tx, msg), 1);
        let _ = registry::take_string(msg);
        let out = ntsc_collections_channel_recv(rx);
        assert_eq!(registry::get_string(out).unwrap(), "ping");
        let _ = registry::take_string(out);
        ntsc_collections_channel_close(tx);
        ntsc_collections_channel_close(rx);
    }

    #[test]
    fn test_channel_try_recv_empty_then_filled() {
        let rx = ntsc_collections_channel(0);
        let tx = ntsc_collections_channel_sender(rx);

        assert_eq!(ntsc_collections_channel_try_recv(rx), 0);
        let msg = put("hello");
        assert_eq!(ntsc_collections_channel_send(tx, msg), 1);
        let _ = registry::take_string(msg);
        let out = ntsc_collections_channel_try_recv(rx);
        assert_eq!(registry::get_string(out).unwrap(), "hello");
        let _ = registry::take_string(out);
        ntsc_collections_channel_close(tx);
        ntsc_collections_channel_close(rx);
    }

    #[test]
    fn test_channel_recv_returns_empty_when_all_senders_closed() {
        let rx = ntsc_collections_channel(4);
        let tx = ntsc_collections_channel_sender(rx);
        let msg = put("only one");
        assert_eq!(ntsc_collections_channel_send(tx, msg), 1);
        let _ = registry::take_string(msg);

        ntsc_collections_channel_close(tx);
        let out = ntsc_collections_channel_recv(rx);
        assert_eq!(registry::get_string(out).unwrap(), "only one");
        let _ = registry::take_string(out);

        let out = ntsc_collections_channel_recv(rx);
        assert_eq!(registry::get_string(out).unwrap(), "");
        let _ = registry::take_string(out);
        ntsc_collections_channel_close(rx);
    }

    #[test]
    fn test_channel_thread_producer() {
        let rx = ntsc_collections_channel(4);
        let tx = ntsc_collections_channel_sender(rx);
        assert!(rx != 0 && tx != 0);
        let producer = std::thread::spawn(move || {
            let msg = put("from thread");
            assert_eq!(ntsc_collections_channel_send(tx, msg), 1);
            let _ = registry::take_string(msg);
            ntsc_collections_channel_close(tx);
        });
        let out = ntsc_collections_channel_recv(rx);
        assert_eq!(registry::get_string(out).unwrap(), "from thread");
        let _ = registry::take_string(out);
        producer.join().unwrap();
        ntsc_collections_channel_close(rx);
    }
}
