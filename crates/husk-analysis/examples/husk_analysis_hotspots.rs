//! Release-build measurements for Husk language-service scaling.

use std::{fs, hint::black_box, sync::Arc, time::Instant};

use anyhow::Result;
use husk_analysis::Workspace;
use husk_runtime::SemanticProfile;

const LOCAL_SYMBOLS: usize = 320;
const WORKSPACE_SYMBOLS: usize = 320;
const COMPLETION_REQUESTS: usize = 32;
const CONFIG_UPDATES: usize = 8;
const DOCUMENT_UPDATES: usize = 16;

fn main() -> Result<()> {
    let scenario = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "husk-completion".to_string());
    anyhow::ensure!(
        matches!(
            scenario.as_str(),
            "husk-completion" | "husk-config" | "husk-update"
        ),
        "unknown Husk performance scenario `{scenario}`"
    );

    let root = tempfile::tempdir()?;
    let local = root.path().join("main.hk");
    let other = root.path().join("other.hk");
    let local_source = (0..LOCAL_SYMBOLS)
        .rev()
        .map(|index| format!("fn local_symbol_{index:04}() {{}}\n"))
        .collect::<String>();
    let other_source = (0..WORKSPACE_SYMBOLS)
        .rev()
        .map(|index| format!("fn workspace_symbol_{index:04}() {{}}\n"))
        .collect::<String>();
    fs::write(&local, &local_source)?;
    fs::write(&other, &other_source)?;
    let mut workspace = Workspace::open(root.path(), SemanticProfile::Native)?;
    anyhow::ensure!(
        workspace.completions(&local, "").len() == LOCAL_SYMBOLS + WORKSPACE_SYMBOLS,
        "Husk benchmark did not index every source symbol"
    );

    let (name, iterations, elapsed) = match scenario.as_str() {
        "husk-completion" => {
            let started = Instant::now();
            for _ in 0..COMPLETION_REQUESTS {
                black_box(workspace.completions(black_box(&local), black_box("")));
            }
            (
                "husk_completion_ranking",
                COMPLETION_REQUESTS,
                started.elapsed(),
            )
        }
        "husk-config" => {
            workspace.set_cfg_flags(["stable-configuration".to_string()]);
            let started = Instant::now();
            for _ in 0..CONFIG_UPDATES {
                workspace.set_cfg_flags(black_box(["stable-configuration".to_string()]));
            }
            (
                "husk_configuration_refresh",
                CONFIG_UPDATES,
                started.elapsed(),
            )
        }
        "husk-update" => {
            let text = Arc::<str>::from(local_source);
            workspace.update(&local, 1, Arc::clone(&text))?;
            let started = Instant::now();
            for index in 0..DOCUMENT_UPDATES {
                workspace.update(&local, (index + 2) as i32, black_box(Arc::clone(&text)))?;
            }
            (
                "husk_unchanged_document_update",
                DOCUMENT_UPDATES,
                started.elapsed(),
            )
        }
        _ => unreachable!("unknown scenario rejected above"),
    };
    println!(
        "[{{\"scenario\":\"{name}\",\"iterations\":{iterations},\"elapsed_us\":{}}}]",
        elapsed.as_micros(),
    );
    Ok(())
}
