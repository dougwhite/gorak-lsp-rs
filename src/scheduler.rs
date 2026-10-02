//! A lightweight transport reader interrupts analysis without cloning the workspace.
//! Only the analysis thread owns documents; edits invalidate older queued queries.
use crate::analysis::{Cancellation, ReferenceSearch};
use crossbeam_channel::{Receiver, Sender, bounded};
use lsp_server::{Connection, ErrorCode, Message, Request, Response};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Interrupt {
    Preempted,
    Modified,
    Cancelled,
}
struct Active {
    id: lsp_server::RequestId,
    priority: u8,
    control: Cancellation,
    interrupted: Option<Interrupt>,
}
impl Active {
    fn interrupt(&mut self, reason: Interrupt) {
        // Explicit cancellation and invalidation take precedence over a retry.
        if self.interrupted.is_none() || self.interrupted == Some(Interrupt::Preempted) {
            self.interrupted = Some(reason);
        }
        self.control.cancel();
    }
}
pub(crate) struct Job {
    pub request: Request,
    revision: u64,
    pub waiting_for_index: bool,
    pub references: Option<ReferenceSearch>,
}
pub(crate) enum Work {
    Request(Job, Cancellation),
    Notification(lsp_server::Notification),
}
struct Queued {
    message: Message,
    revision: u64,
    waiting_for_index: bool,
    references: Option<ReferenceSearch>,
}
#[derive(Default)]
struct State {
    queue: VecDeque<Queued>,
    revision: u64,
    active: Option<Active>,
    disconnected: bool,
}
fn priority(method: &str) -> u8 {
    match method {
        "shutdown" | "gorak/indexStatus" => 0,
        "textDocument/references" | "textDocument/rename" | "workspace/symbol" => 2,
        _ => 1,
    }
}
fn needs_index(method: &str) -> bool {
    matches!(
        method,
        "textDocument/references" | "textDocument/rename" | "workspace/symbol"
    )
}
fn invalidates(method: &str) -> bool {
    matches!(
        method,
        "textDocument/didOpen"
            | "textDocument/didChange"
            | "textDocument/didClose"
            | "workspace/didChangeWatchedFiles"
            | "workspace/didChangeWorkspaceFolders"
    )
}
fn version_sensitive(method: &str) -> bool {
    !matches!(
        method,
        "shutdown" | "gorak/indexStatus" | "gorak/rebuildIndex" | "gorak/builtinSource"
    )
}
fn error(id: lsp_server::RequestId, reason: Interrupt) -> Response {
    let (code, message) = match reason {
        Interrupt::Modified => (
            ErrorCode::ContentModified,
            "Source changed while the request was pending. Retry against the current document.",
        ),
        _ => (ErrorCode::RequestCanceled, "Request cancelled"),
    };
    Response::new_err(id, code as i32, message.into())
}

pub(crate) struct Inbox {
    state: Arc<Mutex<State>>,
    pub wake: Receiver<()>,
    sender: Sender<Message>,
    stop: Sender<()>,
    reader: Option<JoinHandle<()>>,
}
impl Inbox {
    pub fn new(connection: &Connection) -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let (signal, wake) = bounded(1);
        let (stop, stopped) = bounded(1);
        let input = connection.receiver.clone();
        let output = connection.sender.clone();
        let shared = state.clone();
        let reader = thread::spawn(move || {
            loop {
                crossbeam_channel::select! {
                    recv(stopped) -> _ => break,
                    recv(input) -> message => {
                        let mut state = shared.lock().unwrap();
                        match message {
                            Ok(message) => state.receive(message, &output),
                            Err(_) => {
                                state.disconnected = true;
                                if let Some(active) = &mut state.active { active.interrupt(Interrupt::Cancelled); }
                                let _ = signal.try_send(());
                                break;
                            }
                        }
                        let _ = signal.try_send(());
                    }
                }
            }
        });
        Self {
            state,
            wake,
            sender: connection.sender.clone(),
            stop,
            reader: Some(reader),
        }
    }
    pub fn next(&self, index_ready: bool) -> Option<Work> {
        let mut state = self.state.lock().unwrap();
        loop {
            let index = state
                .queue
                .iter()
                .enumerate()
                .filter(|(_, q)| match &q.message {
                    Message::Request(r) => {
                        index_ready || !(q.waiting_for_index || needs_index(&r.method))
                    }
                    _ => true,
                })
                .min_by_key(|(_, q)| match &q.message {
                    Message::Request(r) => priority(&r.method),
                    _ => 0,
                })
                .map(|(i, _)| i)?;
            let queued = state.queue.remove(index).unwrap();
            match queued.message {
                Message::Request(request) => {
                    if queued.revision != state.revision && version_sensitive(&request.method) {
                        let _ = self
                            .sender
                            .send(Message::Response(error(request.id, Interrupt::Modified)));
                        continue;
                    }
                    let control = Cancellation::default();
                    state.active = Some(Active {
                        id: request.id.clone(),
                        priority: priority(&request.method),
                        control: control.clone(),
                        interrupted: None,
                    });
                    return Some(Work::Request(
                        Job {
                            request,
                            revision: queued.revision,
                            waiting_for_index: queued.waiting_for_index,
                            references: queued.references,
                        },
                        control,
                    ));
                }
                Message::Notification(notification) => {
                    return Some(Work::Notification(notification));
                }
                Message::Response(_) => continue,
            }
        }
    }
    /// Response publication and invalidation are serialized with incoming edits.
    /// Interrupted searches retry only after more urgent work has been serviced.
    pub fn finish(&self, job: Job, response: Option<Response>) {
        let mut state = self.state.lock().unwrap();
        let active = state
            .active
            .take()
            .expect("only one analysis request runs at a time");
        match active.interrupted {
            Some(Interrupt::Preempted) => state.queue.push_back(Queued {
                message: Message::Request(job.request),
                revision: job.revision,
                waiting_for_index: job.waiting_for_index,
                references: job.references,
            }),
            Some(reason) => {
                let _ = self
                    .sender
                    .send(Message::Response(error(job.request.id, reason)));
            }
            None => match response {
                Some(response) => {
                    let _ = self.sender.send(Message::Response(response));
                }
                None => state.queue.push_back(Queued {
                    message: Message::Request(job.request),
                    revision: job.revision,
                    waiting_for_index: true,
                    references: job.references,
                }),
            },
        }
    }
    pub fn disconnected(&self) -> bool {
        self.state.lock().unwrap().disconnected
    }
}
impl Drop for Inbox {
    fn drop(&mut self) {
        let _ = self.stop.try_send(());
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
impl State {
    fn receive(&mut self, message: Message, output: &Sender<Message>) {
        match &message {
            Message::Notification(n) if n.method == "$/cancelRequest" => {
                if let Ok(id) =
                    serde_json::from_value::<lsp_server::RequestId>(n.params["id"].clone())
                {
                    if let Some(active) = &mut self.active
                        && active.id == id
                    {
                        active.interrupt(Interrupt::Cancelled);
                    }
                    if let Some(i) = self
                        .queue
                        .iter()
                        .position(|q| matches!(&q.message, Message::Request(r) if r.id == id))
                    {
                        self.queue.remove(i);
                        let _ = output.send(Message::Response(error(id, Interrupt::Cancelled)));
                    }
                }
                return;
            }
            Message::Notification(n) if invalidates(&n.method) => {
                self.revision += 1;
                if let Some(active) = &mut self.active {
                    active.interrupt(Interrupt::Modified);
                }
            }
            Message::Request(r) if r.method == "gorak/rebuildIndex" => {
                self.revision += 1;
                if let Some(active) = &mut self.active {
                    active.interrupt(Interrupt::Modified);
                }
            }
            Message::Request(r) => {
                if let Some(active) = &mut self.active
                    && priority(&r.method) < active.priority
                {
                    active.interrupt(Interrupt::Preempted);
                }
            }
            _ => {}
        }
        if let Message::Notification(next) = &message
            && let Some(last) = self.queue.back_mut()
            && let Message::Notification(previous) = &mut last.message
            && coalesce_changes(previous, next)
        {
            last.revision = self.revision;
            return;
        }
        self.queue.push_back(Queued {
            message,
            revision: self.revision,
            waiting_for_index: false,
            references: None,
        });
    }
}

/// Adjacent full replacements can discard obsolete text. Ranged edits stay
/// separate so the analysis thread can validate each against its accepted version.
fn coalesce_changes(
    previous: &mut lsp_server::Notification,
    next: &lsp_server::Notification,
) -> bool {
    if previous.method != "textDocument/didChange" || previous.method != next.method {
        return false;
    }
    let uri = previous
        .params
        .pointer("/textDocument/uri")
        .and_then(|v| v.as_str());
    if uri.is_none()
        || uri
            != next
                .params
                .pointer("/textDocument/uri")
                .and_then(|v| v.as_str())
    {
        return false;
    }
    let (Some(old), Some(new)) = (
        previous
            .params
            .pointer("/textDocument/version")
            .and_then(|v| v.as_i64()),
        next.params
            .pointer("/textDocument/version")
            .and_then(|v| v.as_i64()),
    ) else {
        return false;
    };
    if new <= old {
        return false;
    }
    let replacement = |params: &serde_json::Value| {
        params["contentChanges"].as_array().is_some_and(|changes| {
            changes.len() == 1
                && changes[0].get("range").is_none_or(|r| r.is_null())
                && changes[0]["text"].is_string()
        })
    };
    if !replacement(&previous.params) || !replacement(&next.params) {
        return false;
    }
    previous.params.clone_from(&next.params);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn harness() -> (Inbox, Receiver<Message>) {
        let (sender, receiver) = crossbeam_channel::unbounded();
        let (stop, _) = bounded(1);
        (
            Inbox {
                state: Arc::new(Mutex::new(State::default())),
                wake: crossbeam_channel::never(),
                sender,
                stop,
                reader: None,
            },
            receiver,
        )
    }
    fn send(inbox: &Inbox, message: Message) {
        inbox.state.lock().unwrap().receive(message, &inbox.sender);
    }
    fn request(id: i32, method: &str) -> Message {
        Message::Request(Request::new(id.into(), method.into(), json!({})))
    }
    fn notification(method: &str, params: serde_json::Value) -> Message {
        Message::Notification(lsp_server::Notification::new(method.into(), params))
    }
    fn take(inbox: &Inbox, ready: bool) -> (Job, Cancellation) {
        let Some(Work::Request(job, token)) = inbox.next(ready) else {
            panic!("expected a request")
        };
        (job, token)
    }
    fn finish(inbox: &Inbox, job: Job) {
        let id = job.request.id.clone();
        inbox.finish(job, Some(Response::new_ok(id, json!(["complete result"]))));
    }
    fn response(output: &Receiver<Message>) -> Response {
        let Message::Response(response) = output.try_recv().unwrap() else {
            panic!("expected response")
        };
        response
    }
    #[test]
    fn completion_preempts_search_and_search_retries_without_partial_response() {
        let (inbox, output) = harness();
        send(&inbox, request(1, "textDocument/references"));
        let (mut search, token) = take(&inbox, true);
        search.references = Some(ReferenceSearch {
            target: crate::analysis::index::Binding {
                document: crate::analysis::index::DocumentId(0),
                symbol: crate::syntax::model::SymbolId(0),
            },
            name: "item".into(),
            next_document: 17,
            locations: Vec::new(),
        });
        send(&inbox, request(2, "textDocument/completion"));
        assert!(token.is_cancelled());
        finish(&inbox, search);
        assert!(output.try_recv().is_err());
        let (completion, _) = take(&inbox, true);
        assert_eq!(completion.request.id, 2.into());
        finish(&inbox, completion);
        let (search, fresh) = take(&inbox, true);
        assert_eq!(search.request.id, 1.into());
        assert!(!fresh.is_cancelled());
        assert_eq!(search.references.as_ref().unwrap().next_document, 17);
        finish(&inbox, search);
        assert_eq!(response(&output).id, 2.into());
        assert_eq!(response(&output).id, 1.into());
        assert!(output.try_recv().is_err());
    }
    #[test]
    fn cancellation_covers_queued_index_waiters_and_running_queries() {
        let (inbox, output) = harness();
        send(&inbox, request(1, "workspace/symbol"));
        assert!(inbox.next(false).is_none());
        send(&inbox, notification("$/cancelRequest", json!({"id":1})));
        assert_eq!(
            response(&output).error.unwrap().code,
            ErrorCode::RequestCanceled as i32
        );
        send(&inbox, request(2, "textDocument/rename"));
        let (job, token) = take(&inbox, true);
        send(&inbox, request(3, "textDocument/hover"));
        send(&inbox, notification("$/cancelRequest", json!({"id":2})));
        assert!(token.is_cancelled());
        finish(&inbox, job);
        let cancelled = response(&output);
        assert_eq!(cancelled.id, 2.into());
        assert_eq!(
            cancelled.error.unwrap().code,
            ErrorCode::RequestCanceled as i32
        );
        assert!(cancelled.result.is_none());
        let (hover, _) = take(&inbox, true);
        finish(&inbox, hover);
        assert!(inbox.next(true).is_none());
    }
    #[test]
    fn edits_reject_stale_rename_and_queued_positions_but_keep_new_queries() {
        let (inbox, output) = harness();
        send(&inbox, request(1, "textDocument/rename"));
        let (rename, token) = take(&inbox, true);
        send(&inbox, request(2, "textDocument/hover"));
        send(&inbox, notification("textDocument/didChange", json!({})));
        send(&inbox, request(3, "textDocument/completion"));
        assert!(token.is_cancelled());
        finish(&inbox, rename);
        assert!(matches!(inbox.next(true), Some(Work::Notification(_))));
        let (completion, _) = take(&inbox, true);
        assert_eq!(completion.request.id, 3.into());
        finish(&inbox, completion);
        for id in [1, 2] {
            let stale = response(&output);
            assert_eq!(stale.id, id.into());
            assert_eq!(stale.error.unwrap().code, ErrorCode::ContentModified as i32);
            assert!(stale.result.is_none());
        }
        assert_eq!(response(&output).id, 3.into());
    }
    #[test]
    fn rebuild_waits_for_index_without_blocking_interactive_queries() {
        let (inbox, output) = harness();
        send(&inbox, request(1, "gorak/rebuildIndex"));
        let (rebuild, _) = take(&inbox, false);
        inbox.finish(rebuild, None);
        send(&inbox, request(2, "textDocument/completion"));
        let (completion, _) = take(&inbox, false);
        finish(&inbox, completion);
        assert!(inbox.next(false).is_none());
        let (rebuild, _) = take(&inbox, true);
        assert!(rebuild.waiting_for_index);
        finish(&inbox, rebuild);
        assert_eq!(response(&output).id, 2.into());
        assert_eq!(response(&output).id, 1.into());
    }
    #[test]
    fn queued_incremental_edits_remain_ordered_and_replacements_discard_old_text() {
        let (inbox, _) = harness();
        let edit = |version, text: &str, full| {
            notification(
                "textDocument/didChange",
                json!({"textDocument":{"uri":"file:///app/main.w4gl", "version":version},"contentChanges":[if full {json!({"text":text})} else {json!({"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"text":text})}]}),
            )
        };
        send(&inbox, edit(2, "first", false));
        send(&inbox, edit(3, "second", false));
        for (version, text) in [(2, "first"), (3, "second")] {
            let Some(Work::Notification(edit)) = inbox.next(true) else {
                panic!("expected separate ranged edit")
            };
            assert_eq!(edit.params["textDocument"]["version"], version);
            assert_eq!(edit.params["contentChanges"][0]["text"], text);
        }
        assert!(inbox.next(true).is_none());
        send(&inbox, edit(4, "discard", true));
        send(&inbox, edit(5, "replacement", true));
        send(&inbox, edit(4, "stale", true));
        let Some(Work::Notification(merged)) = inbox.next(true) else {
            panic!("expected replacement")
        };
        assert_eq!(merged.params["textDocument"]["version"], 5);
        assert_eq!(
            merged.params["contentChanges"],
            json!([{"text":"replacement"}])
        );
        let Some(Work::Notification(stale)) = inbox.next(true) else {
            panic!("stale edits must remain separate for validation")
        };
        assert_eq!(stale.params["textDocument"]["version"], 4);
    }
    #[test]
    fn reader_delivers_cancellation_while_analysis_owns_the_active_request() {
        let (client, server) = Connection::memory();
        let inbox = Inbox::new(&server);
        client
            .sender
            .send(request(1, "textDocument/references"))
            .unwrap();
        inbox
            .wake
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let (job, token) = take(&inbox, true);
        client
            .sender
            .send(notification("$/cancelRequest", json!({"id":1})))
            .unwrap();
        inbox
            .wake
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(token.is_cancelled());
        finish(&inbox, job);
        assert_eq!(
            response(&client.receiver).error.unwrap().code,
            ErrorCode::RequestCanceled as i32
        );
    }
}
