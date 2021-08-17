#![feature(fn_traits)]

use client::RustpadClient;
use crate::editor_binding::{Edit, EditorBinding};
use druid::{Widget, AppLauncher, WindowDesc};
use crate::coeditor::CoEditorWidget;

pub mod client;
pub mod code_editor;
pub mod editor_binding;
pub mod transformer;
pub mod coeditor;

#[tokio::main]
async fn main() {
    let window = WindowDesc::new(build()).title("Rustpad Client");
    let launcher = AppLauncher::with_window(window);
    launcher
        .launch(Default::default())
        .expect("coeditor exited");
}

fn build() -> impl Widget<EditorBinding> {
    CoEditorWidget::new("ws://127.0.0.1:3030/api/socket/abcdef".to_string())
}

