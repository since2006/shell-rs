//! The `shellrs` command as a program of its own. Windows puts a copy of it
//! on the PATH (设置 → 外部 CLI → 安装), because the app itself is a GUI
//! program there and a shell would neither wait for it nor see its output.

fn main() {
    std::process::exit(shellrs::cli::main(std::env::args_os().skip(1).collect()));
}
