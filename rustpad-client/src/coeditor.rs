use druid::{Selector, WidgetPod, WidgetId, ExtEventSink, EventCtx, Event, Env, Widget, UpdateCtx, LayoutCtx, PaintCtx, BoxConstraints, Target, LifeCycle, LifeCycleCtx, Size};
use std::sync::Arc;
use tokio::sync::broadcast::{Sender};
use tokio::task::JoinHandle;
use parking_lot::RwLock;
use crate::{RustpadClient, Edit};
use std::time::Duration;
use crate::editor_binding::EditorBinding;
use crate::code_editor::code_editor::CodeEditor;
use crate::code_editor::text::{Selection, EditableText};
use tokio::sync::broadcast;
use std::collections::HashMap;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::connect_async;
use futures::StreamExt;
use log::{info, warn};

pub const COEDITOR_INIT_CLIENT: Selector<Arc<RwLock<RustpadClient>>> = Selector::new("coeditor-init-client");
pub const USER_EDIT_SELECTOR: Selector<Edit> = Selector::new("user-edit");
pub const USER_CURSOR_UPDATE_SELECTOR: Selector<()> = Selector::new("user-cursor-data");

fn create_connection_loop(client: Arc<RwLock<RustpadClient>>, close_tx: Sender<()>) -> JoinHandle<()> {
    tokio::spawn(async move {
        info!("connecting");
        let conn = client.read().server_url.clone();
        loop {
            let x = Arc::clone(&client);
            if try_connect(&conn, x, close_tx.clone()).await.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1000)).await;
            warn!("Reconnecting ...");
        }
    })
}

async fn try_connect(connect_addr: &String, client: Arc<RwLock<RustpadClient>>, close_tx: Sender<()>) -> Option<()> {
    let url = url::Url::parse(connect_addr).unwrap();

    let (ws_tx, ws_rx) = futures_channel::mpsc::unbounded::<Message>();

    client.write().ws_sender = Some(ws_tx.clone());
    client.write().users.clear();
    //
    let res = connect_async(url).await;
    if res.is_err() {
        eprintln!("{:?}", res.err().unwrap());
        return Some(());
    }

    let (ws_stream, _) = res.unwrap();

    println!("WebSocket handshake has been successfully completed");
    client.read().on_connected.invoke(());

    let (write, read) = ws_stream.split();
    let websocket_tx = ws_rx.map(Ok).forward(write);

    let client2 = Arc::clone(&client);
    let receive_handler =
        read.for_each(|message| async {
            if message.is_err() {
                return;
            }
            let data = message.unwrap().to_string();
            println!("Received: {}", &data);
            client2.write().handle_message(serde_json::from_slice(data.as_bytes()).expect("parse data failed"));
        });

    client.write().send_info();
    client.write().send_cursor_data();

    if let Some(outstanding) = &client.read().outstanding {
        client.write().send_operation(outstanding);
    }

    let mut close_rx = close_tx.subscribe();

    tokio::select! {
        _ = close_rx.recv() => {
            ws_tx.unbounded_send(Message::Close(None)).unwrap();
            println!("client closed.");
            return None;
        }
        _ = websocket_tx => {}
        _ = receive_handler => {
            println!("server closed");
        }
    }
    println!("{} disconnected", &connect_addr);

    client.write().ws_sender = None;

    Some(())
}

pub struct CoEditorWidget {
    inner: WidgetPod<EditorBinding, CodeEditor<EditorBinding>>,
    id: WidgetId,
    pub server_url: String,
    client: Option<Arc<RwLock<RustpadClient>>>,
    connection_handle: Option<JoinHandle<()>>,
    event_sink: Option<ExtEventSink>,
    close_tx: Sender<()>,
    last_selection: Selection,
}

impl Drop for CoEditorWidget {
    fn drop(&mut self) {
        self.close_tx.send(()).unwrap();
        futures::executor::block_on(
            tokio::time::timeout(Duration::from_secs(5),
                                 self.connection_handle.take().unwrap(),
            )
        ).unwrap();
        println!("CoEditorWidget destructed");
    }
}

impl CoEditorWidget {
    pub fn new(server_url: String) -> Self {
        println!("CoEditorWidget created");
        CoEditorWidget {
            inner: WidgetPod::new(CodeEditor::<EditorBinding>::multiline()),
            server_url,
            id: WidgetId::next(),
            client: None,
            connection_handle: None,
            event_sink: None,
            close_tx: broadcast::channel(1).0,
            last_selection: Selection::default(),
        }
    }
}

impl Widget<EditorBinding> for CoEditorWidget {
    fn event(&mut self, ctx: &mut EventCtx, event: &Event, data: &mut EditorBinding, env: &Env) {
        if let Event::Command(cmd) = event {
            println!("received {:?}", cmd);
        }
        match event {
            Event::Command(command) if command.get(COEDITOR_INIT_CLIENT).is_some() => {
                // && command.target() == Target::Widget(self.inner.id())
                let client = command.get(COEDITOR_INIT_CLIENT).unwrap();
                data.set_client(client);
                println!("editor binding client initialized");
            }
            Event::Command(command) if command.get(USER_EDIT_SELECTOR).is_some() => {
                println!("received edit command");
                let edit = command.get(USER_EDIT_SELECTOR).unwrap();
                let selection = self.inner.widget().text().borrow().selection();

                let transform_selection = |selection: Selection| -> Selection {
                    let transform_index = |x: usize| -> usize {
                        if x < edit.begin {
                            x
                        } else if x > edit.end {
                            x + edit.begin + edit.content.len() - edit.end
                        } else {
                            edit.begin + edit.content.len()
                        }
                    };

                    Selection::new(
                        transform_index(selection.anchor),
                        transform_index(selection.active),
                    )
                };

                data.edit_without_callback(edit);
                let _ = self.inner.widget_mut().text_mut().borrow_mut().set_selection(transform_selection(selection));
                self.inner.widget_mut().text_mut().borrow_mut().decorations.iter_mut()
                    .for_each(|(_, b)| *b = transform_selection(b.clone()));
            }
            Event::Command(command) if command.get(USER_CURSOR_UPDATE_SELECTOR).is_some() => {
                println!("received cursor command");
                let content = &data.content;
                let unicode_offset_to_utf8_offset = |offset: u32| -> usize {
                    content.iter().take(offset as usize).collect::<String>().len()
                };
                let mut new_decorations = HashMap::new();
                let my_id = self.client.as_ref().unwrap().read().id();
                self.client.as_ref().unwrap().read().user_cursors.iter()
                    .filter(|(&id, _)| id != my_id)
                    .filter(|(_, data)| !data.selections.is_empty())
                    .for_each(|(&id, sel)| {
                        new_decorations.insert(id, Selection::new(
                            unicode_offset_to_utf8_offset(sel.selections[0].0),
                            unicode_offset_to_utf8_offset(sel.selections[0].1),
                        ));
                    });
                self.inner.widget_mut().text_mut().borrow_mut().decorations = new_decorations;
            }
            _ => self.inner.event(ctx, event, data, env)
        }
    }

    fn lifecycle(&mut self, ctx: &mut LifeCycleCtx, event: &LifeCycle, data: &EditorBinding, env: &Env) {
        self.inner.lifecycle(ctx, event, data, env);
        match event {
            LifeCycle::WidgetAdded => {
                self.id = ctx.widget_id();
                println!("CoEditorWidget initialized with id: {:?}", self.id);
                self.event_sink = Some(ctx.get_external_handle());

                let client = RustpadClient::create(self.server_url.clone());
                client.write().widget_id = Some(self.id);
                client.write().set_event_sink(
                    self.event_sink.as_ref().unwrap().clone(),
                    self.id,
                );

                self.client = Some(Arc::clone(&client));

                ctx.get_external_handle().submit_command(
                    COEDITOR_INIT_CLIENT,
                    Box::new(Arc::clone(&client)),
                    Target::Widget(self.id),
                ).expect("send command failed");

                self.connection_handle = Some(create_connection_loop(client, self.close_tx.clone()));
            }
            _ => {}
        }
    }

    fn update(&mut self, ctx: &mut UpdateCtx, _old_data: &EditorBinding, data: &EditorBinding, env: &Env) {
        let new_selection = self.inner.widget().text().borrow().selection();
        if self.last_selection != new_selection {
            self.last_selection = new_selection;
            let borrow = self.inner.widget_mut().text_mut().borrow_mut();
            let content = &borrow.layout.text().unwrap().content_as_string;
            let active = content.slice(0..new_selection.active).unwrap_or_default().to_string().chars().count() as u32;
            let anchor = content.slice(0..new_selection.anchor).unwrap_or_default().to_string().chars().count() as u32;
            self.client.as_ref().unwrap().write().update_and_send_cursor_data((active, anchor));
        }
        self.inner.update(ctx, data, env);
    }

    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints, data: &EditorBinding, env: &Env) -> Size {
        self.inner.layout(ctx, bc, data, env)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, data: &EditorBinding, env: &Env) {
        self.inner.paint(ctx, data, env)
    }
}
