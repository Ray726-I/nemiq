use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "nemiq", version, about = "A focused Markdown writing app")]
pub struct Cli {
    /// Markdown file to open
    pub file: Option<String>,

    /// Start with vim-style modal editing enabled
    #[arg(long)]
    pub vim: bool,

    /// Convert the input file to PDF or HTML and exit (no window)
    #[arg(long, value_name = "FILE")]
    pub export: Option<String>,
}
