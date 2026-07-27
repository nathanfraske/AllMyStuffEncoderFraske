//! Launch one long-lived sandbox process with durable file-backed stdio.
//!
//! PowerShell's `Start-Process -RedirectStandardOutput` can keep the calling
//! PowerShell alive while descendants retain the redirected handles. This
//! helper opens ordinary files, gives those handles to the child, prints the
//! child's PID, and exits. The launched process is then independent of the
//! bootstrap terminal while all of its descendants inherit the same logs.

use std::{
    env,
    fs::OpenOptions,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(windows)]
const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
#[cfg(windows)]
const STD_INPUT_HANDLE: u32 = -10_i32 as u32;
#[cfg(windows)]
const STD_OUTPUT_HANDLE: u32 = -11_i32 as u32;
#[cfg(windows)]
const STD_ERROR_HANDLE: u32 = -12_i32 as u32;

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetStdHandle(standard_handle: u32) -> *mut std::ffi::c_void;
    fn SetHandleInformation(
        object: *mut std::ffi::c_void,
        mask: u32,
        flags: u32,
    ) -> i32;
}

#[derive(Debug)]
struct Launch {
    cwd: PathBuf,
    stdout: PathBuf,
    stderr: PathBuf,
    program: PathBuf,
    arguments: Vec<String>,
}

fn usage() -> &'static str {
    "usage: sandbox_process_launcher \
        --cwd DIR --stdout FILE --stderr FILE -- PROGRAM [ARG ...]"
}

fn next_value(
    arguments: &mut impl Iterator<Item = String>,
    option: &str,
) -> Result<PathBuf, String> {
    arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("{option} requires a value"))
}

fn parse() -> Result<Launch, String> {
    let mut arguments = env::args().skip(1);
    let mut cwd = None;
    let mut stdout = None;
    let mut stderr = None;
    let mut program = None;
    let mut child_arguments = Vec::new();

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--cwd" => cwd = Some(next_value(&mut arguments, "--cwd")?),
            "--stdout" => {
                stdout = Some(next_value(&mut arguments, "--stdout")?)
            }
            "--stderr" => {
                stderr = Some(next_value(&mut arguments, "--stderr")?)
            }
            "--" => {
                program = Some(
                    arguments
                        .next()
                        .map(PathBuf::from)
                        .ok_or_else(|| "`--` must be followed by a program".to_owned())?,
                );
                child_arguments.extend(arguments);
                break;
            }
            unknown => return Err(format!("unknown argument: {unknown}")),
        }
    }

    Ok(Launch {
        cwd: cwd.ok_or_else(|| "--cwd is required".to_owned())?,
        stdout: stdout.ok_or_else(|| "--stdout is required".to_owned())?,
        stderr: stderr.ok_or_else(|| "--stderr is required".to_owned())?,
        program: program.ok_or_else(|| "a program is required after `--`".to_owned())?,
        arguments: child_arguments,
    })
}

fn open_log(path: &Path) -> Result<std::fs::File, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "could not create log directory {}: {error}",
                parent.display()
            )
        })?;
    }
    OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("could not open log {}: {error}", path.display()))
}

#[cfg(windows)]
fn make_inherited_console_handles_private() -> Result<(), String> {
    for standard_handle in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: GetStdHandle returns a borrowed process handle. We neither
        // close it nor retain it. SetHandleInformation only clears its inherit
        // flag, which does not affect this process's ability to use the handle.
        let handle = unsafe { GetStdHandle(standard_handle) };
        if handle.is_null() || handle as isize == -1 {
            continue;
        }
        if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
            return Err(format!(
                "could not clear inheritance on standard handle {standard_handle}: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

fn run() -> Result<(), String> {
    let launch = parse()?;
    let stdout = open_log(&launch.stdout)?;
    let stderr = open_log(&launch.stderr)?;

    #[cfg(windows)]
    make_inherited_console_handles_private()?;

    let mut command = Command::new(&launch.program);
    command
        .args(&launch.arguments)
        .current_dir(&launch.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let child = command.spawn().map_err(|error| {
        format!(
            "could not launch sandbox process {}: {error}",
            launch.program.display()
        )
    })?;
    println!("{{\"pid\":{}}}", child.id());
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        eprintln!("{}", usage());
        std::process::exit(1);
    }
}
