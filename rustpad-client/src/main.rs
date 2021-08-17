#![feature(fn_traits)]

use futures::Stream;
use futures::stream::StreamFuture;
use futures_util::StreamExt;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::{Notify, broadcast};
use tokio::task::JoinHandle;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};

use std::sync::Arc;
use parking_lot::RwLock;
use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender};

use std::time::Duration;
use tokio::io::BufReader;

pub mod client;
pub mod code_editor;
pub mod editor;
pub mod transformer;

use client::RustpadClient;
use crate::editor::{Edit, EditorProxy, EditorBinding, Edit1};
use druid::widget::{Controller, Checkbox, Either, Slider, Label, Flex};
use druid::{Target, WidgetId, Widget, Data, Lens, LifeCycleCtx, LifeCycle, UpdateCtx, WidgetPod, LayoutCtx, BoxConstraints, Size, PaintCtx, ExtEventSink};

use log::{info, warn};
use druid::{WidgetExt, EventCtx, Event, Env, AppLauncher, WindowDesc, Selector};
use crate::code_editor::code_editor::CodeEditor;
use broadcast::Sender;
use crate::code_editor::text::{Selection, ImeInvalidation, EditableText};
use druid_shell::text::Event::SelectionChanged;
use rustpad_server::rustpad::CursorData;
use std::collections::HashMap;
use std::num::NonZeroU64;

const COEDITOR_INIT_CLIENT: Selector<Arc<RwLock<RustpadClient>>> = Selector::new("coeditor-init-client");
const USER_EDIT_SELECTOR: Selector<Edit> = Selector::new("user-edit");
const USER_CURSOR_UPDATE_SELECTOR: Selector<()> = Selector::new("user-cursor-data");

fn create_connection_loop(client: Arc<RwLock<RustpadClient>>, close_tx: Sender<()>) -> JoinHandle<()> {
    tokio::spawn(async move {
        info!("connecting");
        let conn = client.read().server_url.clone();
        loop {
            let x = Arc::clone(&client);
            let mut recv = close_tx.subscribe();
            tokio::select! {
                res = try_connect(&conn, x, close_tx.clone()) => {
                    if res.is_none() {
                        break;
                    }
                },
                _ = recv.recv() => {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(1000)).await;
            warn!("Reconnecting ...");
        }
    })
}

#[tokio::main]
async fn main() {
    pretty_env_logger::init();
    let widget = CoEditorWidget::new("ws://127.0.0.1:3030/api/socket/abcdef".to_string());
    let window = WindowDesc::new(widget).title("Rustpad Client");
    let launcher = AppLauncher::with_window(window);
    launcher
        .launch(Default::default())
        .expect("coeditor exited");
}

fn build_edit(offset: usize, content: String) -> Edit {
    Edit {
        begin: offset,
        end: offset,
        content,
    }
}

async fn try_connect(connect_addr: &String, client: Arc<RwLock<RustpadClient>>, close_tx: Sender<()>) -> Option<()> {
    let url = url::Url::parse(connect_addr).unwrap();

    let (mpsc_sender, mpsc_receiver) = futures_channel::mpsc::unbounded::<Message>();
    let (shutdown_sender, shutdown_receiver) = futures_channel::mpsc::unbounded::<()>();
    // let (sender, _receiver) = broadcast::channel(1);
    // send data from stdin
    // let stdin = tokio::spawn(read_stdin(Arc::clone(&client), shutdown_sender, sender.clone()));

    client.write().ws_sender = Some(mpsc_sender.clone());
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
    let websocket_sender = mpsc_receiver.map(Ok).forward(write);

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

    // pin_mut!(websocket_sender, receive_handler);
    let mut close_rx = close_tx.subscribe();

    tokio::select! {
        _ = close_rx.recv() => {
            mpsc_sender.unbounded_send(Message::Close(None));
            println!("client closed.");
            return None;
        }
        _ = websocket_sender => {}
        _ = receive_handler => {
            println!("server closed");
        }
    }
    println!("{} disconnected", &connect_addr);

    client.write().ws_sender = None;
    // stdin.abort();

    Some(())
}

async fn read_stdin(client: Arc<RwLock<RustpadClient>>, notify: UnboundedSender<()>, close_signal: Sender<()>) {
    let mut x = BufReader::new(tokio::io::stdin()).lines();
    loop {
        print!("> ");
        let current_handle = x.next_line();
        let mut close_signal = close_signal.subscribe();

        tokio::select! {
            line_content = current_handle => {
                let current = line_content.unwrap().unwrap();
                println!("entered {}\n", current);
                if current == "quit" {
                    notify.unbounded_send(()).unwrap();
                    break;
                }
                let push_back = build_edit(
                    client.read().editor_binding.get_content().len(),
                    current,
                );
                info!("edited {:?}", push_back);
                let push_back2 = push_back.clone();
                client.write().event_sink.as_ref().map(move |x| {
                    x.submit_command(USER_EDIT_SELECTOR, Box::new(push_back), Target::Auto).unwrap();
                });
                for callback in &client.write().editor_binding.after_edits {
                    callback.invoke(&push_back2);
                }
            },
            _ = close_signal.recv() => {
                info!("cli exited gracefully");
                break;
            }
        }

        // client.write().editor_binding.edit_content(push_back2);
    }
}

struct CoEditorWidget {
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
        self.close_tx.send(());
        futures::executor::block_on(
            tokio::time::timeout(Duration::from_secs(5),
                                 self.connection_handle.take().unwrap()
            )
        );
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
            last_selection: Selection::default()
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
            },
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
                    .for_each(|(a, b)| *b = transform_selection(b.clone()));
            },
            Event::Command(command) if command.get(USER_CURSOR_UPDATE_SELECTOR).is_some() => {
                println!("received cursor command");
                let content = &data.content;
                let unicode_offset_to_utf8_offset = |offset: u32| -> usize {
                    content.iter().take(offset as usize).collect::<String>().len()
                };
                let mut new_decorations = HashMap::new();
                let my_id = self.client.as_ref().unwrap().read().my_id.unwrap();
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
            },
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
                    self.id
                );

                self.client = Some(Arc::clone(&client));

                ctx.get_external_handle().submit_command(
                    COEDITOR_INIT_CLIENT,
                    Box::new(Arc::clone(&client)),
                    Target::Widget(self.id)
                    // Target::Auto,
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
            let active = content.slice(0 .. new_selection.active).unwrap_or_default().to_string().chars().count() as u32;
            let anchor = content.slice(0 .. new_selection.anchor).unwrap_or_default().to_string().chars().count() as u32;
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
