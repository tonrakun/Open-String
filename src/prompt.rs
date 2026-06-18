use std::io::Write;

/// Prompts the user with a yes/no question on stdin/stdout. Shared by the
/// god mode reconfirmation flow (`main.rs`) and the Mediator's
/// `ConfirmationPrompt` implementation (`agent::confirmation`).
pub fn yes_no(prompt: &str) -> std::io::Result<bool> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let answer = input.trim().to_lowercase();
    Ok(answer == "y" || answer == "yes")
}
