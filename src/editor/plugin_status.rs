//! The `:plugin-status` view of each configured plugin's session state.

use super::*;
use crate::plugin::{quarantine_reason, PluginSessionState, PluginStatus, PluginStatusRow};
use crate::ui::MIN_HORIZONTAL_PREVIEW_PANE_WIDTH;

/// Text previews clip long lines, so wrap them to fit the narrowest side-by-side pane.
const PREVIEW_WRAP_WIDTH: usize = MIN_HORIZONTAL_PREVIEW_PANE_WIDTH - 2;

impl Editor {
    /// Lists every configured plugin with its state, problems first.
    pub(super) fn open_plugin_status(&mut self) {
        let mut rows = self.plugin_registry.status_rows();
        if rows.is_empty() {
            self.set_routine_foreground_message(
                Severity::Info,
                "no plugins are configured".to_string(),
            );
            return;
        }
        rows.sort_by(|left, right| {
            left.state
                .cmp(&right.state)
                .then_with(|| left.name.cmp(&right.name))
        });
        let problems = rows
            .iter()
            .filter(|row| {
                matches!(
                    row.state,
                    PluginSessionState::Quarantined | PluginSessionState::ReloadRejected
                )
            })
            .count();
        let actions = rows
            .iter()
            .map(|row| (row.name.clone(), plugin_status_action(row)))
            .collect::<HashMap<_, _>>();
        let items = rows.iter().map(plugin_status_item).collect();
        let status = match problems {
            0 => format!("{} plugins", rows.len()),
            1 => format!("{} plugins · 1 problem", rows.len()),
            count => format!("{} plugins · {count} problems", rows.len()),
        };
        let picker = Picker::builder()
            .title("Plugin status")
            .structured_items(items)
            .item_layout(PickerItemLayout::LabelFirst)
            .filter_action(diagnostics_picker::diagnostic_filter_score)
            .placeholder("Filter plugins")
            .status(status)
            .history_key("plugin-status")
            .select_action(move |item| {
                actions
                    .get(&item)
                    .cloned()
                    .unwrap_or_else(|| Action::Print("plugin is no longer configured".to_string()))
            })
            .build(self);
        self.current_dialog = Some(Box::new(picker));
    }
}

fn plugin_status_reason(row: &PluginStatusRow) -> Option<(&str, &str)> {
    match &row.status {
        PluginStatus::Quarantined {
            stage, diagnostic, ..
        } => Some((stage, diagnostic)),
        PluginStatus::ActiveWithReloadError { diagnostic, .. } => Some(("reload", diagnostic)),
        _ => None,
    }
}

fn plugin_status_item(row: &PluginStatusRow) -> PickerItem {
    let kind = match row.state {
        PluginSessionState::Quarantined => "Error",
        PluginSessionState::ReloadRejected | PluginSessionState::Starting => "Warning",
        PluginSessionState::Active => "Success",
        PluginSessionState::Pending
        | PluginSessionState::LoadsOnFirstUse
        | PluginSessionState::Disabled => "Info",
    };
    let reason = plugin_status_reason(row);
    let detail = reason.map_or_else(
        || row.path.clone(),
        |(stage, diagnostic)| format!("{stage}: {}", quarantine_reason(diagnostic)),
    );
    let mut preview = format!(
        "Plugin: {}\nState: {}\nSource: {}",
        row.name,
        row.state.label(),
        row.path
    );
    if let Some((stage, diagnostic)) = reason {
        preview.push_str(&format!("\nStage: {stage}\n\n{diagnostic}"));
    }
    match row.state {
        PluginSessionState::Quarantined => preview.push_str(
            "\n\nRed stopped this plugin for the rest of the session. Restart Red to load it \
             again. If you are developing this plugin, saving a fix reloads it.",
        ),
        PluginSessionState::ReloadRejected => preview.push_str(
            "\n\nThe previous version is still running because the new one failed to load.",
        ),
        PluginSessionState::Starting => {
            preview.push_str("\n\nRed is waiting for this plugin to finish its startup handshake.")
        }
        PluginSessionState::LoadsOnFirstUse => preview
            .push_str("\n\nThis plugin loads the first time one of its commands or events runs."),
        _ => {}
    }
    PickerItem {
        id: row.name.clone(),
        icon: None,
        label: row.name.clone(),
        kind: Some(kind.to_string()),
        annotation: Some(row.state.label().to_string()),
        detail: Some(detail.clone()),
        data: json!({
            "search_text": format!("{} {} {detail}", row.name, row.state.label()),
        }),
        matches: Vec::new(),
        detail_matches: Vec::new(),
        preview: Some(PickerPreview::Text {
            text: wrap_preview(&preview, PREVIEW_WRAP_WIDTH),
            language: None,
        }),
    }
}

/// Wraps each line to `width` columns, continuing it at the same indentation.
fn wrap_preview(text: &str, width: usize) -> String {
    text.lines()
        .flat_map(|line| {
            let indent = &line[..line.len() - line.trim_start().len()];
            let options = textwrap::Options::new(width).subsequent_indent(indent);
            textwrap::wrap(line, options)
                .into_iter()
                .map(std::borrow::Cow::into_owned)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn plugin_status_action(row: &PluginStatusRow) -> Action {
    let message = match plugin_status_reason(row) {
        Some((_, diagnostic)) => format!(
            "{}: {} ({})",
            row.name,
            row.state.label(),
            quarantine_reason(diagnostic)
        ),
        None => format!("{}: {}", row.name, row.state.label()),
    };
    Action::Print(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(state: PluginSessionState, status: PluginStatus) -> PluginStatusRow {
        PluginStatusRow {
            name: "reader".to_string(),
            path: "/plugins/reader.hk".to_string(),
            state,
            status,
        }
    }

    #[test]
    fn quarantined_plugin_shows_its_stage_and_full_reason() {
        let item = plugin_status_item(&row(
            PluginSessionState::Quarantined,
            PluginStatus::Quarantined {
                stage: "activation".to_string(),
                path: "/plugins/reader.hk".to_string(),
                diagnostic: "RPC request `initialize` timed out after 5000 ms\nmore detail"
                    .to_string(),
            },
        ));

        assert_eq!(item.kind.as_deref(), Some("Error"));
        assert_eq!(item.annotation.as_deref(), Some("quarantined"));
        assert_eq!(
            item.detail.as_deref(),
            Some("activation: RPC request `initialize` timed out after 5000 ms")
        );
        let Some(PickerPreview::Text { text, .. }) = item.preview else {
            panic!("expected a text preview");
        };
        assert!(text.contains("Stage: activation"), "{text}");
        assert!(text.contains("more detail"), "{text}");
        assert!(text.contains("Restart Red to load"), "{text}");
        assert!(
            text.lines()
                .all(|line| line.chars().count() <= PREVIEW_WRAP_WIDTH),
            "{text}"
        );
    }

    #[test]
    fn healthy_plugins_show_their_source() {
        let item = plugin_status_item(&row(PluginSessionState::Active, PluginStatus::Active));
        assert_eq!(item.kind.as_deref(), Some("Success"));
        assert_eq!(item.detail.as_deref(), Some("/plugins/reader.hk"));

        let lazy = plugin_status_item(&row(
            PluginSessionState::LoadsOnFirstUse,
            PluginStatus::Pending,
        ));
        assert_eq!(lazy.annotation.as_deref(), Some("loads on first use"));
        assert_eq!(lazy.kind.as_deref(), Some("Info"));
    }

    #[test]
    fn wrapped_preview_lines_keep_their_indentation() {
        let wrapped = wrap_preview("  --> a/very/long/path/to/a/plugin.hk:1:18\nshort", 20);

        assert!(
            wrapped.lines().all(|line| line.chars().count() <= 20),
            "{wrapped}"
        );
        assert!(
            wrapped.lines().take(2).all(|line| line.starts_with("  ")),
            "{wrapped}"
        );
        assert_eq!(wrapped.lines().last(), Some("short"));
    }
}
