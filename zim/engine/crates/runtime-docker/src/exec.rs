//! The one-shot `exec` capability (docker exec via bollard).

use std::time::Duration;

use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
use bollard::Docker;
use futures_util::StreamExt;

use zim_engine_application::supervision::{ExecCommand, ExecOutput, RuntimeError};

use crate::map_error;

/// Run `cmd` inside the instance's container and collect its output
/// and exit code, bounded by `timeout`.
pub(crate) async fn exec(
    docker: &Docker,
    container: &str,
    cmd: &ExecCommand,
    timeout: Duration,
) -> Result<ExecOutput, RuntimeError> {
    let env: Vec<String> = cmd
        .env
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();

    let create = CreateExecOptions {
        cmd: Some(cmd.cmd.clone()),
        env: if env.is_empty() { None } else { Some(env) },
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        attach_stdin: Some(false),
        tty: Some(false),
        user: cmd.user.clone(),
        working_dir: cmd.working_dir.clone(),
        ..Default::default()
    };
    let created = docker
        .create_exec(container, create)
        .await
        .map_err(|e| map_error("exec create", e))?;

    let started = docker
        .start_exec(
            &created.id,
            Some(StartExecOptions {
                detach: false,
                tty: false,
                output_capacity: Some(64 * 1024),
            }),
        )
        .await
        .map_err(|e| map_error("exec start", e))?;

    let mut result = ExecOutput::default();
    match started {
        StartExecResults::Attached {
            output: stream,
            input: _input,
        } => {
            let mut stream = stream;
            // Drain the output stream, bounded by the exec timeout.
            let drain = async {
                while let Some(chunk) = stream.next().await {
                    match chunk {
                        Ok(log) => match log {
                            bollard::container::LogOutput::StdOut { message } => {
                                result.stdout.extend_from_slice(&message);
                            }
                            bollard::container::LogOutput::StdErr { message } => {
                                result.stderr.extend_from_slice(&message);
                            }
                            // Console demuxes; StdIn never appears on
                            // an exec output stream.
                            other => {
                                result.stdout.extend_from_slice(other.as_ref());
                            }
                        },
                        Err(e) => return Err(map_error("exec output", e)),
                    }
                }
                Ok(())
            };
            tokio::time::timeout(timeout, drain)
                .await
                .map_err(|_| RuntimeError::Timeout("exec output stream timed out".to_owned()))??;
        }
        StartExecResults::Detached => {
            return Err(RuntimeError::Backend(
                "exec started detached despite non-detach options".to_owned(),
            ));
        }
    }

    let inspected = docker
        .inspect_exec(&created.id)
        .await
        .map_err(|e| map_error("exec inspect", e))?;
    result.exit_code = inspected.exit_code;
    Ok(result)
}
