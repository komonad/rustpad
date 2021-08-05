#![feature(fn_traits)]

use futures_util::{future, pin_mut, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use operational_transform::{OperationSeq, Operation};
use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;
use futures_channel::mpsc::UnboundedSender;

use rustpad_server::rustpad::{UserInfo, ClientMsg, ServerMsg, CursorData, UserOperation};
use std::time::Duration;
use tokio::io::BufReader;
use std::mem::{swap, take};

pub mod client;
pub mod code_editor;
pub mod editor;
pub mod transformer;

use client::RustpadClient;
use crate::editor::{Edit, EditorProxy, EditorBinding};
use tokio::sync::Notify;
use druid::widget::{TextBox, Controller, WidgetWrapper};
use druid::{Target, WidgetId, Widget};

use log::{info, error, warn};
use druid::{WidgetExt, EventCtx, Event, Env, LifeCycleCtx, LifeCycle, UpdateCtx, AppLauncher, WindowDesc, Selector};
use std::ops::Range;
use std::borrow::Cow;
use crate::code_editor::code_editor::CodeEditor;
use crate::client::Callback;
use std::num::NonZeroU64;

fn test_send<T: Send>(x: &T) {}

// struct EditData {
//     content: String,
//     chars: Vec<char>,
//     senders: Vec<UnboundedSender<(*const String, *const Edit)>>,
// }

const USER_EDIT_SELECTOR: Selector<Edit> = Selector::new("user-edit");

struct EditorController(WidgetId);

impl Controller<EditorBinding, CodeEditor<EditorBinding>> for EditorController {
    fn event(&mut self, child: &mut CodeEditor<EditorBinding>, ctx: &mut EventCtx, event: &Event, data: &mut EditorBinding, env: &Env) {
        match event {
            Event::Command(command) if command.get(USER_EDIT_SELECTOR).is_some() => {
                data.edit_without_callback(command.get(USER_EDIT_SELECTOR).unwrap());
            }
            _ => child.event(ctx, event, data, env),
        };
    }
}

#[tokio::main]
async fn main() {
    let connect_addr = "ws://127.0.0.1:3030/api/socket/abcdef".to_string();
    let editor_widget_id: WidgetId = WidgetId::next();

    let client = RustpadClient::create().await;

    let client2 = Arc::clone(&client);

    let editor = CodeEditor::multiline()
        .controller(EditorController(editor_widget_id))
        .with_id(editor_widget_id);

    let mut our_document = EditorBinding::default();

    let client3 = Arc::clone(&client);
    our_document.on_edit(Callback::new(Arc::new(RwLock::new(move |b: *const Edit| unsafe {
        // GUI edit to Rustpad Client
        if let Some(mut handle) = client3.try_write() {
            handle.editor_binding.edit_without_callback(&*b);
            handle.on_change((*b).clone())
        }
    }))));
    // editor.text().borrow_mut().layout.set_text(our_document);

    let window = WindowDesc::new(editor).title("Rustpad Client");
    let launcher = AppLauncher::with_window(window);

    let event_sink = launcher.get_external_handle();
    let event_sink2 = launcher.get_external_handle();

    client.write().editor_binding.on_edit(Callback::new(Arc::new(RwLock::new(move |b: *const Edit| unsafe {
        // Rustpad Client edit to GUI
        event_sink.submit_command(USER_EDIT_SELECTOR, Box::new((*b).clone()), Target::Widget(editor_widget_id)).unwrap();
    }))));

    // let document: Arc<RwLock<EditorBinding>> = Default::default();

    // editor.text().borrow_mut().layout.set_text();

    let connect_loop = tokio::spawn(async move {
        println!("connecting");
        let conn = connect_addr;
        let client = client2;
        loop {
            let x = Arc::clone(&client);
            let res = try_connect(&conn, x).await;
            if res.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1000)).await;
            warn!("Reconnecting ...");
        }
    });

    launcher
        .launch(our_document)
        .unwrap();

    connect_loop.abort();
}

fn build_edit(offset: usize, content: String) -> Edit {
    Edit {
        begin: offset,
        end: offset,
        content,
    }
}


async fn try_connect(connect_addr: &String, client: Arc<RwLock<RustpadClient>>) -> Option<()> {
    let url = url::Url::parse(connect_addr).unwrap();

    let (mpsc_sender, mpsc_receiver) = futures_channel::mpsc::unbounded::<Message>();

    // let editor = Arc::clone(&client.read().editor_binding);
    //
    let (shutdown_sender, shutdown_receiver) = futures_channel::mpsc::unbounded();
    // send data from stdin
    let stdin = tokio::spawn(read_stdin(Arc::clone(&client), shutdown_sender));

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
    let receive_handler = {
        read.for_each(|message| async {
            if message.is_err() {
                return;
            }
            let data = message.unwrap().to_string();
            println!("Received: {}", &data);
            client2.write().handle_message(serde_json::from_slice(data.as_bytes()).expect("parse data failed"));
        })
    };

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

async fn read_stdin(client: Arc<RwLock<RustpadClient>>, notify: UnboundedSender<()>) {
    let mut x = BufReader::new(tokio::io::stdin()).lines();
    loop {
        print!("> ");
        let current = x.next_line().await.unwrap().unwrap_or_default();
        tokio::io::stdout().write_all(format!("entered {}\n", current).as_bytes()).await.unwrap();
        if current == "quit" {
            notify.unbounded_send(());
            break;
        }
        let push_back = build_edit(
            client.read().editor_binding.get_content().len(),
            current,
        );
        info!("edited {:?}", push_back);
        client.write().event_sink.as_ref().map(move |x| {
            x.submit_command(USER_EDIT_SELECTOR, Box::new(push_back), Target::Auto);
        });
    }
}
