use editor::Editor;

mod editor;

fn main() -> anyhow::Result<()> {
    let mut editor = Editor::new()?;
    editor.run()
}
