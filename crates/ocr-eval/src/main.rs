use std::{fs::File, io::Read, process::ExitCode};
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.len() != 2 {
        return Err("usage: ocr-eval MANIFEST RESULTS".into());
    }
    let read = |path: &str| -> Result<String, Box<dyn std::error::Error>> {
        let mut text = String::new();
        File::open(path)?
            .take(16 * 1024 * 1024 + 1)
            .read_to_string(&mut text)?;
        Ok(text)
    };
    let report = ocr_eval::score_benchmark(&read(&arguments[0])?, &read(&arguments[1])?)?;
    serde_json::to_writer_pretty(std::io::stdout().lock(), &report)?;
    Ok(())
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
