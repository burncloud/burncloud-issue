mod app;
mod codex;
mod github;
mod models;
mod pipeline;
mod ui;

use std::{io, path::PathBuf, time::Duration};

use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use app::App;

#[derive(Debug, Parser)]
#[command(name = "burncloud-issue")]
#[command(about = "Issue dependency tree and execution-status console for BurnCloud")]
struct Args {
    /// GitHub repository whose Issue/PR task graph is displayed and managed.
    #[arg(long, default_value = "burncloud/burncloud")]
    repo: String,

    /// Local read-only source repository that Codex may inspect when drafting a bounded Issue.
    #[arg(long, default_value = "../burncloud")]
    local_repo: PathBuf,

    /// Optional Codex model override. Omit to use the local Codex configured model.
    #[arg(long)]
    codex_model: Option<String>,

    /// Maximum seconds for each Codex turn.
    #[arg(long, default_value_t = 300)]
    codex_timeout_secs: u64,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut app = App::new(
        args.repo,
        args.local_repo,
        args.codex_model,
        Duration::from_secs(args.codex_timeout_secs),
    )?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    loop {
        app.tick();
        terminal.draw(|frame| ui::draw(frame, app))?;
        if app.should_quit {
            return Ok(());
        }

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat {
                    app.handle_key(key);
                }
            }
        }
    }
}
