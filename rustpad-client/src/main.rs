#![feature(fn_traits)]

use futures_util::StreamExt;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};

use std::sync::Arc;
use parking_lot::RwLock;
use futures_channel::mpsc::UnboundedSender;

use std::time::Duration;
use tokio::io::BufReader;

pub mod client;
pub mod code_editor;
pub mod editor;
pub mod transformer;

use client::RustpadClient;
use crate::editor::{Edit, EditorProxy, EditorBinding};
use druid::widget::Controller;
use druid::{Target, WidgetId, Widget};

use log::{info, warn};
use druid::{WidgetExt, EventCtx, Event, Env, AppLauncher, WindowDesc, Selector};
use crate::code_editor::code_editor::CodeEditor;
use crate::client::Callback;

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
    pretty_env_logger::init();

    let editor_widget_id: WidgetId = WidgetId::next();

    let editor =
        CodeEditor::multiline()
        .controller(EditorController(editor_widget_id))
        .with_id(editor_widget_id);

    let window = WindowDesc::new(editor).title("Rustpad Client");
    let launcher = AppLauncher::with_window(window);

    let client = RustpadClient::create();
    client.write().set_event_sink(&launcher, editor_widget_id);

    let client2 = Arc::clone(&client);
    let connect_loop = tokio::spawn(async move {
        info!("connecting");
        let conn = "ws://127.0.0.1:3030/api/socket/abcdef".to_string();
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
        .launch(EditorBinding::create_with_client(&client))
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
        // client.write().editor_binding.edit_content(push_back2);
    }
}
