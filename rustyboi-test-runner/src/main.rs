//! Entry point for the suite runner. The logic lives in the library
//! (`rustyboi_test_runner_lib::app`) so the other bins and the test suite can
//! link against it.

fn main() -> std::process::ExitCode {
    rustyboi_test_runner_lib::app::main()
}
