fn main() {
    let exit_code = match ddl::run_cli(std::env::args_os()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("ddl: {error}");
            1
        }
    };

    std::process::exit(exit_code);
}
