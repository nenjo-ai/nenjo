use std::time::{Duration, Instant};

use console::style;
use eyre::Result;
use indicatif::{ProgressBar, ProgressStyle};

pub fn run_phase<T>(
    message: impl Into<String>,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let message = message.into();
    let started = Instant::now();
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(spinner_style());
    spinner.enable_steady_tick(Duration::from_millis(80));
    spinner.set_message(message.clone());

    match operation() {
        Ok(value) => {
            spinner.finish_and_clear();
            eprintln!(
                "{} {} {}",
                style("✓").green(),
                message,
                style(format_duration(started.elapsed())).dim()
            );
            Ok(value)
        }
        Err(error) => {
            spinner.finish_and_clear();
            eprintln!("{} {}", style("✗").red(), message);
            Err(error)
        }
    }
}

pub fn print_header(title: &str) {
    eprintln!("{}", style(title).bold());
}

pub fn print_success(message: impl AsRef<str>) {
    eprintln!("{} {}", style("◆").green(), message.as_ref());
}

pub fn print_note(message: impl AsRef<str>) {
    eprintln!("{} {}", style("◇").cyan(), message.as_ref());
}

fn spinner_style() -> ProgressStyle {
    ProgressStyle::with_template("{spinner:.cyan} {msg}")
        .expect("static spinner template should be valid")
        .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
}

fn format_duration(duration: Duration) -> String {
    let millis = duration.as_millis();
    if millis < 1_000 {
        format!("{millis}ms")
    } else {
        format!("{:.1}s", duration.as_secs_f64())
    }
}
