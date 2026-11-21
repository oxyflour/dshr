use std::sync::{mpsc, Arc, Mutex, TryLockError};
use std::time::Duration;

use super::{read_agent_presets_from, ChatState};

const POLL_INTERVAL: Duration = Duration::from_millis(500);

pub(super) fn start(state: &Arc<Mutex<ChatState>>) -> Option<mpsc::Receiver<()>> {
    if state.lock().ok()?.preset_inputs.is_none() {
        return None;
    }
    let state = Arc::downgrade(state);
    let (updates, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut sample = None;
        loop {
            std::thread::sleep(POLL_INTERVAL);
            let Some(state) = state.upgrade() else {
                break;
            };
            let mut state = match state.try_lock() {
                Ok(state) => state,
                Err(TryLockError::WouldBlock) => continue,
                Err(TryLockError::Poisoned(_)) => break,
            };
            let current = read_agent_presets_from(state.preset_inputs.as_ref());
            if sample.as_ref() != Some(&current) {
                sample = Some(current);
                continue;
            }
            if state.published_presets.as_ref() == Some(&current) {
                continue;
            }
            state.publish_agent_presets(current);
            state.bind_agent_preset();
            drop(state);
            if updates.send(()).is_err() {
                break;
            }
        }
    });
    Some(receiver)
}
