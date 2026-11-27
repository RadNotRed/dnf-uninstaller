mod cleanup;
mod desktop;
mod inventory;
mod managed;
mod packages;
mod updates;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if let [flag, package, plan] = args.as_slice()
        && flag == "--apply-rpm"
    {
        return packages::apply_rpm_as_root(package, plan);
    }
    if let [flag, version] = args.as_slice()
        && flag == "--apply-update"
    {
        return updates::apply_as_root(version);
    }
    if rustix::process::geteuid().is_root() {
        return Err("Run the desktop application as a regular user.".into());
    }
    if !args.is_empty() {
        return Err("Unknown arguments.".into());
    }
    desktop::run()
}
