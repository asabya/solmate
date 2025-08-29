use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// In a real scenario, this would likely be `solana_sdk::transaction::TransactionError`
// or a similar struct. For this function, we just need something that can be serialized.
pub type TransactionError = Value;

/// Holds the parsed logs and metadata for a single program instruction invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionLogs {
    /// The address of the program that was invoked.
    pub invoked_program: String,
    /// The raw log lines associated with this specific invocation.
    pub logs: Vec<String>,
    /// The number of compute units consumed by this instruction.
    pub compute_units: u64,
    /// A boolean flag indicating whether the instruction failed.
    pub failure: bool,
}

// Regex to identify the start of a program invocation.
// Captures: 1 = Program Address/ID
static INVOKE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"Program (\S+) invoke \[\d+\]").unwrap());

// Regex to identify the end of a program invocation and its result.
// Captures: 1 = Program Address/ID, 2 = "success" or "failed..."
static RESULT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"Program (\S+) (success|failed.*)").unwrap());

// Regex to extract the compute units consumed by a program.
// Captures: 1 = Consumed Units
static CONSUMED_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"Program .* consumed (\d+) of \d+ compute units").unwrap());

/// Parses raw Solana transaction logs into a structured format, organized by program instruction.
///
/// This is a Rust implementation inspired by the TypeScript `parseProgramLogs` function
/// in the Solana Explorer, with improvements for robustness against malformed or incomplete logs.
///
/// # Arguments
///
/// * `logs` - A slice of strings, where each string is a log line from a transaction.
/// * `error` - An optional `TransactionError` from the transaction's metadata. If the
///           JSON representation of this error appears in a log, the corresponding
///           instruction is marked as a failure.
/// * `program_addresses_to_keep` - A slice of program address strings to filter the results by.
///                                 Only logs from these programs will be returned.
///
/// # Returns
///
/// A `Vec<InstructionLogs>` containing the parsed and filtered logs for each instruction.
/// The instructions are ordered with the most deeply nested ones appearing first.
pub fn parse_program_logs(
    logs: &[&str],
    error: Option<&TransactionError>,
    program_addresses_to_keep: &[&str],
) -> Vec<InstructionLogs> {
    let mut invocation_stack: Vec<InstructionLogs> = Vec::new();
    let mut completed_instructions: Vec<InstructionLogs> = Vec::new();

    let error_string = error.and_then(|e| serde_json::to_string(e).ok());

    for log in logs {
        // Case 1: A new program invocation starts. A new frame is pushed to the stack.
        if let Some(caps) = INVOKE_RE.captures(log) {
            let program_address = caps.get(1).unwrap().as_str().to_string();
            invocation_stack.push(InstructionLogs {
                invoked_program: program_address,
                logs: vec![log.to_string()],
                compute_units: 0,
                failure: false,
            });
            continue;
        }

        // Case 2: The currently executing program finishes (or an outer one does).
        if let Some(caps) = RESULT_RE.captures(log) {
            let program_address = caps.get(1).unwrap().as_str();
            let result_text = caps.get(2).unwrap().as_str();

            // Unwind the stack until we find the program that just finished.
            // This robustly handles cases where an outer program's failure causes
            // inner calls to be aborted without their own failure logs.
            while let Some(mut instruction) = invocation_stack.pop() {
                let is_matching_program = instruction.invoked_program == program_address;

                // Associate the final result log with the instruction it belongs to.
                if is_matching_program {
                    instruction.logs.push(log.to_string());
                }

                // The "consumed" log line is usually near the end.
                // We check all logs for the instruction just in case the order varies.
                if let Some(consumed) = instruction
                    .logs
                    .iter()
                    .rev()
                    .find_map(|l| CONSUMED_RE.captures(l))
                {
                    instruction.compute_units = consumed
                        .get(1)
                        .unwrap()
                        .as_str()
                        .parse::<u64>()
                        .unwrap_or(0);
                }

                // Determine failure status.
                if is_matching_program {
                    // This is the instruction that explicitly finished.
                    let mut is_failure = result_text.starts_with("failed");
                    if !is_failure {
                        if let Some(ref err_str) = error_string {
                            if instruction.logs.iter().any(|l| l.contains(err_str)) {
                                is_failure = true;
                            }
                        }
                    }
                    instruction.failure = is_failure;
                } else {
                    // This is an inner instruction that was forcefully unwound. Mark as failed.
                    instruction.failure = true;
                }

                completed_instructions.push(instruction);

                if is_matching_program {
                    break; // Stop unwinding once the correct instruction is found and processed.
                }
            }
            continue;
        }

        // Case 3: An intermediate log line (e.g., "Program log: ...").
        // Add it to the log records of the currently executing instruction.
        if let Some(current_instruction) = invocation_stack.last_mut() {
            current_instruction.logs.push(log.to_string());
        }
    }

    // Any instructions left on the stack at the end are considered failed because they
    // never explicitly logged a success or failure (e.g., due to transaction timeout).
    while let Some(mut unclosed_instruction) = invocation_stack.pop() {
        unclosed_instruction.failure = true;
        completed_instructions.push(unclosed_instruction);
    }

    // Filter the collected logs to only include the programs the caller is interested in.
    completed_instructions
        .into_iter()
        .filter(|log| program_addresses_to_keep.contains(&log.invoked_program.as_str()))
        .collect()
}