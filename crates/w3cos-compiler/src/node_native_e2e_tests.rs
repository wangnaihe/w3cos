//! Native-module replacements from `w3cos-node` (M3 item 20): each fixture
//! under `fixtures/node_native/` is AOT-compiled and must print what Node
//! 20.19 prints with the real native package installed
//! (`@vscode/sqlite3` 5.1.14-vscode, `@vscode/spdlog` 0.15.8).

use crate::cjs_e2e_tests::{compile_and_run_with_env, write_fixture};

fn run(name: &str, source: &str) -> Vec<String> {
    let crate_name = format!("w3cos_node_native_{name}");
    let root = write_fixture(
        &crate_name,
        &[
            ("package.json", r#"{"name":"node-native"}"#),
            ("main.js", source),
        ],
    );
    let lines = compile_and_run_with_env(&root, &root.join("main.js"), &crate_name, &[]);
    std::fs::remove_dir_all(root).ok();
    lines
}

const SQLITE3_NODE: &str = r#"constants 1 2 4 function
open null function true
event open
exec null
finalize undefined
run null 3 1
update null 1
commit null
all null [["a","uno"],["b","two"],["c",[1,2,3]]]
integrity null {"integrity_check":"ok"}
get null {"n":3,"f":1.5,"z":null}
get missing null undefined
error SQLITE_ERROR: no such table: Nope SQLITE_ERROR 1 undefined
bad insert SQLITE_ERROR
each row a
each row b
each row c
each done null 3
close null
reopen null a,b,c
readonly SQLITE_READONLY
done
"#;

#[test]
fn vscode_sqlite3_matches_node_sqlite3() {
    let lines = run("sqlite3", include_str!("fixtures/node_native/sqlite3.js"));
    let expected: Vec<&str> = SQLITE3_NODE.lines().collect();
    assert_eq!(lines, expected);
}

const SPDLOG_NODE: &str = r#"api number function function function
level 2
default pattern
  [<time>] [main] [debug] debug line
  [<time>] [main] [error] an error
  [<time>] [main] [critical] critical!
patterns
  <time> [info] vscode pattern
  [main] W | short level %
rotate before main.1.log,main.2.log,main.log [<time>] [main] [debug] debug line / [<time>] [main] [error] an error / [<time>] [main] [critical] critical!
rotate after main.1.log,main.2.log,main.log
newest [main] I | rotation filler line 8 % / [main] I | rotation filler line 9 % / [main] I | rotation filler line 10 % / [main] I | rotation filler line 11 %
older [main] I | rotation filler line 3 % / [main] I | rotation filler line 4 % / [main] I | rotation filler line 5 % / [main] I | rotation filler line 6 % / [main] I | rotation filler line 7 %
oldest <time> [info] vscode pattern / [main] W | short level % / [main] I | rotation filler line 0 % / [main] I | rotation filler line 1 % / [main] I | rotation filler line 2 %
off false
async info async info
done
"#;

#[test]
fn vscode_spdlog_matches_native_spdlog() {
    let lines = run("spdlog", include_str!("fixtures/node_native/spdlog.js"));
    let expected: Vec<&str> = SPDLOG_NODE.lines().collect();
    assert_eq!(lines, expected);
}
