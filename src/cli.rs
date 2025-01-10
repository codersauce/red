use clap::Parser;

#[derive(Debug, Parser)]
pub struct Args {
    /// Root path
    #[clap(short, long)]
    pub root: Option<String>,

    /// Files to edit
    pub files: Vec<String>,
}
