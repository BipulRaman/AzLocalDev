//! Wires [`super::command::Command`] dispatch to [`super::state::EntityState`] methods,
//! running as a background tokio task per entity (queue/subscription) - the actual "actor"
//! in "actor task". [`spawn_entity`] is the only public entry point, returning an
//! [`super::handle::EntityHandle`] the rest of the crate uses to talk to it.

use std::sync::Arc;
use tokio::sync::{mpsc, Notify};

use crate::model::{EntityKind, EntityOptions};

use super::command::Command;
use super::handle::EntityHandle;
use super::state::EntityState;

pub fn spawn_entity(name: String, kind: EntityKind, options: EntityOptions) -> EntityHandle {
    let (tx, mut rx) = mpsc::channel::<Command>(1024);
    let notify = Arc::new(Notify::new());
    let notify_for_task = notify.clone();
    let handle_name = Arc::new(name.clone());

    tokio::spawn(async move {
        let mut state = EntityState::new(name, kind, options);
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(250));
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let had_active = !state.active.is_empty();
                    state.tick();
                    if !had_active && !state.active.is_empty() {
                        notify_for_task.notify_waiters();
                    }
                }
                cmd = rx.recv() => {
                    let Some(cmd) = cmd else { break };
                    match cmd {
                        Command::Send { msg, reply } => {
                            let seq = state.enqueue(msg);
                            notify_for_task.notify_waiters();
                            let _ = reply.send(Ok(seq));
                        }
                        Command::TryReceive { mode, reply } => {
                            let msg = state.try_receive(mode);
                            let _ = reply.send(Ok(msg));
                        }
                        Command::AcceptSession { requested, reply } => {
                            let granted = state.accept_session(requested);
                            let _ = reply.send(granted);
                        }
                        Command::ReleaseSession { session_id } => {
                            state.release_session(&session_id);
                        }
                        Command::TryReceiveSession { session_id, mode, reply } => {
                            let msg = state.try_receive_session(&session_id, mode);
                            let _ = reply.send(Ok(msg));
                        }
                        Command::Complete { lock_token, reply } => {
                            let _ = reply.send(state.complete(lock_token));
                        }
                        Command::Abandon { lock_token, reply } => {
                            let res = state.abandon(lock_token);
                            if res.is_ok() {
                                notify_for_task.notify_waiters();
                            }
                            let _ = reply.send(res);
                        }
                        Command::DeadLetter { lock_token, reason, description, reply } => {
                            let _ = reply.send(state.dead_letter(lock_token, reason, description));
                        }
                        Command::Defer { lock_token, reply } => {
                            let _ = reply.send(state.defer(lock_token));
                        }
                        Command::RenewLock { lock_token, reply } => {
                            let _ = reply.send(state.renew_lock(lock_token));
                        }
                        Command::Peek { state: msg_state, from_sequence, max_count, reply } => {
                            let _ = reply.send(state.peek(msg_state, from_sequence, max_count));
                        }
                        Command::Purge { reply } => {
                            let _ = reply.send(state.purge());
                        }
                        Command::Delete { sequence_number, reply } => {
                            let _ = reply.send(state.delete_message(sequence_number));
                        }
                        Command::Resubmit { sequence_number, reply } => {
                            let res = state.resubmit_dead_letter(sequence_number);
                            if res.is_ok() {
                                notify_for_task.notify_waiters();
                            }
                            let _ = reply.send(res);
                        }
                        Command::Stats { reply } => {
                            let _ = reply.send(state.stats());
                        }
                        Command::Export { reply } => {
                            let _ = reply.send(state.export());
                        }
                        Command::Restore { dump, reply } => {
                            state.restore(dump);
                            notify_for_task.notify_waiters();
                            let _ = reply.send(());
                        }
                        Command::Tick => {
                            state.tick();
                        }
                    }
                }
            }
        }
    });

    EntityHandle::new(handle_name, tx, notify)
}
