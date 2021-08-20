use rustpad_server::rustpad::{ServerMsg, UserOperation, UserInfo, CursorData, ClientMsg};
use std::mem::swap;
use operational_transform::{OperationSeq, Operation};
use std::collections::HashMap;
use futures_channel::mpsc::UnboundedSender;
use tokio_tungstenite::tungstenite::Message;
use std::sync::Arc;
use parking_lot::RwLock;

use log::{trace, info, error};

use crate::coeditor::{USER_EDIT_SELECTOR, USER_CURSOR_UPDATE_SELECTOR};
use crate::editor_binding::{EditorBinding, EditorProxy, Edit};
use crate::transformer::IndexTransformer;
use druid::{ExtEventSink, Target, WidgetId};

#[derive(Clone)]
pub struct Callback<T> {
    callable: Arc<RwLock<dyn Fn(T) -> () + Send + Sync>>,
}

impl<T> PartialEq for Callback<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.callable, &other.callable)
    }
}

impl<T> Callback<T> {
    pub fn new(callable: Arc<RwLock<dyn Fn(T) -> () + Send + Sync>>) -> Self {
        Callback {
            callable
        }
    }

    pub(crate) fn invoke(&self, arg: T) {
        (self.callable.write())(arg)
    }
}

impl<T> Default for Callback<T> {
    fn default() -> Self {
        Callback {
            callable: Arc::new(RwLock::new(|_| { return; }))
        }
    }
}

pub struct RustpadClient {
    pub server_url: String,
    pub widget_id: Option<WidgetId>,
    buffer: Option<OperationSeq>,
    pub outstanding: Option<OperationSeq>,
    revision: usize,
    my_id: Option<u64>,
    pub users: HashMap<u64, UserInfo>,
    pub user_cursors: HashMap<u64, CursorData>,
    my_info: Option<UserInfo>,
    last_value: String,
    pub ws_sender: Option<UnboundedSender<Message>>,
    language: String,
    pub cursor_data: CursorData,
    on_change_user: Callback<()>,
    pub on_connected: Callback<()>,
    on_change_language: Callback<(String, String)>,
    during_operation_application: bool,
    pub editor_binding: EditorBinding,
    pub event_sink: Option<ExtEventSink>,
    // pub(crate) editor_proxy: Arc<RwLock<dyn EditorProxy>>,
}

impl Default for RustpadClient {
    fn default() -> Self {
        RustpadClient {
            server_url: "".to_string(),
            widget_id: None,
            buffer: None,
            outstanding: None,
            revision: 0,
            my_id: None,
            users: Default::default(),
            user_cursors: Default::default(),
            my_info: None,
            last_value: "".to_string(),
            ws_sender: None,
            language: "".to_string(),
            cursor_data: Default::default(),
            on_change_user: Default::default(),
            on_connected: Default::default(),
            on_change_language: Default::default(),
            during_operation_application: false,
            editor_binding: Default::default(),
            event_sink: None
        }
    }
}

impl RustpadClient {
    pub(crate) fn create(server_url: String) -> Arc<RwLock<Self>> {
        let res = Arc::new(RwLock::new(RustpadClient {
            server_url,
            my_info: Some(UserInfo {
                name: "Comonad".to_string(),
                hue: 1231231231,
            }),
            on_change_user: Callback::new(Arc::new(RwLock::new(|_| {
                info!("user changed");
            }))),
            on_connected: Callback::new(Arc::new(RwLock::new(|_| {
                info!("connected");
            }))),
            on_change_language: Callback::new(Arc::new(RwLock::new(|(old, new)| {
                info!("language set from {} to {}", old, new);
            }))),
            ..RustpadClient::default()
        }));
        res
    }
}


impl RustpadClient {
    pub fn set_event_sink(&mut self, event_sink: ExtEventSink, editor_widget_id: WidgetId) {
        // let event_sink = launcher.get_external_handle();
        self.event_sink = Some(event_sink.clone());
        self.editor_binding.on_edit(Callback::new(Arc::new(RwLock::new(move |b: *const Edit| unsafe {
            println!("send rustpad client edit to widget");
            // Rustpad Client edit to GUI
            event_sink.submit_command(USER_EDIT_SELECTOR, Box::new((*b).clone()), Target::Widget(editor_widget_id))
                .expect("client edit sent to widget");
        }))));
    }

    pub(crate) fn send_info(&mut self) -> Option<()> {
        self.ws_sender.as_ref()?.unbounded_send(Message::Text(
            serde_json::to_string(&ClientMsg::ClientInfo(
                self.my_info.as_ref().unwrap().clone()
            )).unwrap()
        )).ok()
    }

    pub(crate) fn send_operation(&self, operation: &OperationSeq) -> Option<()> {
        self.ws_sender.as_ref()?.unbounded_send(Message::Text(
            format!(
                "{{ \"Edit\": {{ \"revision\": {}, \"operation\": {} }} }}",
                self.revision,
                serde_json::to_string(operation).unwrap()
            )
        )).ok()
    }

    pub fn update_and_send_cursor_data(&mut self, selection: (u32, u32)) -> Option<()> {
        self.cursor_data = CursorData {
            cursors: vec![selection.0],
            selections: vec![(std::cmp::min(selection.0, selection.1), std::cmp::max(selection.1, selection.0))]
        };
        self.send_cursor_data()
    }

    pub(crate) fn send_cursor_data(&mut self) -> Option<()> {
        self.ws_sender.as_ref()?.unbounded_send(Message::Text(
            format!(
                "{{ \"CursorData\": {} }}",
                serde_json::to_string(&self.cursor_data).unwrap()
            )
        )).ok()
    }

    fn send_server_ack(&mut self) {
        if self.outstanding.is_none() {
            error!("received server ack without outstanding operation");
        }
        self.outstanding = self.buffer.take();
        if let Some(x) = &self.outstanding {
            self.send_operation(x);
        }
    }

    pub fn close(&mut self) -> Option<()> {
        self.ws_sender.as_ref()?.unbounded_send(Message::Close(None)).ok()
    }

    pub fn set_id(&mut self, id: u64) {
        self.my_id = Some(id);
    }

    pub fn id_is(&self, id: u64) -> bool {
        self.my_id == Some(id)
    }

    pub fn id(&self) -> u64 {
        self.my_id.unwrap()
    }

    fn transform_cursor(&mut self, operation: &OperationSeq) {
        for value in self.user_cursors.values_mut() {
            value.cursors.iter_mut().for_each(|x| {
                *x = operation.transform_index(*x);
            });
            value.selections.iter_mut().for_each(|(begin, end)| {
                *begin = operation.transform_index(*begin);
                *end = operation.transform_index(*end);
            });
        }
        self.update_cursors();
    }

    fn apply_server(&mut self, operation: &OperationSeq) {
        println!("apply server operation {:?}", operation);
        if let Some(outstanding) = &self.outstanding {
            let (t1, t2) = outstanding.transform(&operation).unwrap();
            self.outstanding = Some(t1);
            if let Some(buffer) = &self.buffer {
                let (t3, t4) = buffer.transform(&operation).unwrap();
                self.buffer = Some(t3);
                self.apply_operation(&t4);
            } else {
                self.apply_operation(&t2);
            }
        } else {
            self.apply_operation(&operation);
        }
    }

    fn apply_operation(&mut self, operation: &OperationSeq) {
        trace!("now apply operation seq {:?}", operation);
        if operation.is_noop() {
            return;
        }
        self.during_operation_application = true;

        let mut current_position: usize = 0;

        for op in operation.ops() {
            trace!("applying operation {:?}", op);
            match op {
                Operation::Delete(len) => {
                    self.editor_binding.edit_content(Edit {
                        begin: current_position,
                        end: current_position + *len as usize,
                        content: "".to_string(),
                    });
                }
                Operation::Retain(len) => {
                    current_position += *len as usize;
                }
                Operation::Insert(content) => {
                    self.editor_binding.edit_content(Edit {
                        begin: current_position,
                        end: current_position,
                        content: content.clone(),
                    })
                }
            }
        }

        self.last_value = self.editor_binding.get_content();
        self.during_operation_application = false;
        self.transform_cursor(operation);
    }

    pub fn on_change(&mut self, edit: Edit) {
        if self.during_operation_application {
            return;
        }
        let content = &self.last_value;
        let length = content.chars().count() as u64;

        println!("length: {}, edit: {:?}", length, edit);

        let Edit { begin, end, content } = edit;
        let mut new_op = OperationSeq::default();
        new_op.retain(begin as u64);
        new_op.delete((end - begin) as u64);
        new_op.insert(&content);
        new_op.retain(length - end as u64);
        self.apply_client(new_op);
        self.last_value = self.editor_binding.content_as_string.clone();
        println!("self.last_value = {}", self.last_value);
    }

    fn apply_client(&mut self, operation: OperationSeq) {
        trace!("trying to apply operation {:?}", operation);
        if self.outstanding.is_none() {
            trace!("sending operation {:?} to server", operation);
            self.send_operation(&operation);
            trace!("sent operation {:?} to server", operation);
            self.outstanding = Some(operation.clone());
        } else if self.buffer.is_none() {
            self.buffer = Some(operation.clone());
        } else {
            self.buffer = Some(self.buffer.take().unwrap().compose(&operation).unwrap());
        }
        trace!("trying to transform cursor by {:?}", operation);
        self.transform_cursor(&operation);
        trace!("transformed cursor by {:?}", operation);
    }

    fn update_cursors(&mut self) {
        self.event_sink.as_ref().unwrap().submit_command(
            USER_CURSOR_UPDATE_SELECTOR,
            Box::new(()),
            Target::Widget(self.widget_id.unwrap()),
            // Target::Auto,
        ).expect("cursor update send failed");
    }

    fn update_users(&mut self) {
        // do nothing
    }

    pub(crate) fn handle_message(&mut self, message: ServerMsg) {
        match message {
            ServerMsg::Identity(id) => {
                self.set_id(id)
            }
            ServerMsg::History { start, operations } => {
                if start > self.revision {
                    eprintln!("History start {} > revision {}", start, self.revision);
                    self.close();
                    return;
                }
                for i in self.revision - start..operations.len() {
                    let UserOperation { id, operation } = &operations[i];
                    self.revision += 1;
                    if self.id_is(*id) {
                        self.send_server_ack();
                    } else {
                        self.apply_server(operation);
                    }
                }
            }
            ServerMsg::Language(mut lang) => {
                swap(&mut self.language, &mut lang);
                self.on_change_language.invoke((lang, self.language.clone()));
            }
            ServerMsg::UserInfo { id, info } => {
                if Some(id) != self.my_id && info.is_some() {
                    self.users.insert(id, info.unwrap());
                    self.update_users();
                    self.on_change_user.invoke(());
                }
            }
            ServerMsg::UserCursor { id, data } => {
                if Some(id) != self.my_id {
                    self.user_cursors.insert(id, data);
                    self.update_cursors();
                }
            }
        }
    }
}
