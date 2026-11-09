use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "nemiq", version, about = "A focused Markdown writing app")]
pub struct Cli {
    /// Markdown file to open
    pub file: Option<String>,
}
