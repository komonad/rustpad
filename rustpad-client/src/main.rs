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
use crate::editor::{Edit, EditorProxy, EditorBinding};
use druid::widget::{Controller, Checkbox, Either, Slider, Label, Flex};
use druid::{Target, WidgetId, Widget, Data, Lens, LifeCycleCtx, LifeCycle, UpdateCtx};

use log::{info, warn};
use druid::{WidgetExt, EventCtx, Event, Env, AppLauncher, WindowDesc, Selector};
use crate::code_editor::code_editor::CodeEditor;
use broadcast::Sender;
use crate::code_editor::text::{Selection, ImeInvalidation, EditableText};
use druid_shell::text::Event::SelectionChanged;
use rustpad_server::rustpad::CursorData;

const USER_EDIT_SELECTOR: Selector<Edit> = Selector::new("user-edit");

struct EditorController(WidgetId, Arc<RwLock<RustpadClient>>, Selection);

impl Controller<EditorBinding, CodeEditor<EditorBinding>> for EditorController {
    fn event(&mut self, child: &mut CodeEditor<EditorBinding>, ctx: &mut EventCtx, event: &Event, data: &mut EditorBinding, env: &Env) {
        match event {
            Event::Command(command) if command.get(USER_EDIT_SELECTOR).is_some() => {
                let edit = command.get(USER_EDIT_SELECTOR).unwrap();
                let selection = child.text_mut().borrow_mut().selection();

                let transform_index = |x: usize| -> usize {
                    if x < edit.begin {
                        x
                    } else if x > edit.end {
                        x + edit.begin + edit.content.len() - edit.end
                    } else {
                        edit.begin + edit.content.len()
                    }
                };

                let new_selection = Selection::new(
                    transform_index(selection.anchor),
                    transform_index(selection.active),
                );

                data.edit_without_callback(edit);
                let _ = child.text_mut().borrow_mut().set_selection(new_selection);
            }
            _ => child.event(ctx, event, data, env),
        };
    }

    fn update(&mut self, child: &mut CodeEditor<EditorBinding>, ctx: &mut UpdateCtx, old_data: &EditorBinding, data: &EditorBinding, env: &Env) {
        let new_selection = child.text().borrow().selection();
        if self.2 != new_selection {
            self.2 = new_selection;
            let borrow = child.text_mut().borrow_mut();
            let content = &borrow.layout.text().unwrap().content_as_string;
            let active = content.slice(0 .. new_selection.active).unwrap_or_default().to_string().chars().count() as u32;
            let anchor = content.slice(0 .. new_selection.anchor).unwrap_or_default().to_string().chars().count() as u32;
            self.1.write().update_and_send_cursor_data((active, anchor));
        }
        child.update(ctx, old_data, data, env);
    }
}

fn create_connection_loop(client: Arc<RwLock<RustpadClient>>, close: Sender<()>) -> JoinHandle<()> {
    tokio::spawn(async move {
        info!("connecting");
        let conn = "ws://127.0.0.1:3030/api/socket/abcdef".to_string();
        let close = close;
        loop {
            let x = Arc::clone(&client);
            let mut recv = close.subscribe();
            tokio::select! {
                res = try_connect(&conn, x, close.clone()) => {
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

    let editor_widget_id: WidgetId = WidgetId::next();
    let client = RustpadClient::create();

    let editor =
        CodeEditor::multiline()
            .controller(EditorController(editor_widget_id, Arc::clone(&client), Selection::default()))
            .with_id(editor_widget_id);

    let window = WindowDesc::new(editor).title("Rustpad Client");
    let launcher = AppLauncher::with_window(window);


    client.write().set_event_sink(&launcher, editor_widget_id);

    let (close, _) = broadcast::channel(1);
    let connect_loop = create_connection_loop(Arc::clone(&client), close.clone());

    launcher
        .launch(EditorBinding::create_with_client(&client))
        .unwrap();

    close.send(()).unwrap();
    connect_loop.await.expect("cli exited gracefully");
}

fn build_edit(offset: usize, content: String) -> Edit {
    Edit {
        begin: offset,
        end: offset,
        content,
    }
}

async fn try_connect(connect_addr: &String, client: Arc<RwLock<RustpadClient>>, sender: Sender<()>) -> Option<()> {
    let url = url::Url::parse(connect_addr).unwrap();

    let (mpsc_sender, mpsc_receiver) = futures_channel::mpsc::unbounded::<Message>();
    let (shutdown_sender, shutdown_receiver) = futures_channel::mpsc::unbounded();
    // let (sender, _receiver) = broadcast::channel(1);
    // send data from stdin
    let stdin = tokio::spawn(read_stdin(Arc::clone(&client), shutdown_sender, sender.clone()));

    client.write().ws_sender = Some(mpsc_sender);
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

    // pin_mut!(websocket_sender, receive_handler, shutdown_receiver);
    let shutdown = shutdown_receiver.into_future();
    //
    tokio::select! {
        _ = shutdown => {
            println!("client closed.");
            return None;
        }
        _ = websocket_sender => {}
        _ = receive_handler => {
            println!("server closed");
        }
    }
    // // future::select(websocket_sender, receive_handler).await;
    println!("{} disconnected", &connect_addr);

    client.write().ws_sender = None;
    stdin.abort();

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
