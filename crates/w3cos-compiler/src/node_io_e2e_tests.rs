//! `child_process` and `net` from `w3cos-node` (M3 item 19, slice 1): each
//! fixture under `fixtures/node_io/` is AOT-compiled and must print what
//! Node 20.19 prints for the same file (`node fixtures/node_io/<name>.js`;
//! the permission fixture under `node --experimental-permission
//! --allow-fs-read=*`, which w3cos matches with `W3COS_NODE_DENY=child`).

use crate::cjs_e2e_tests::{compile_and_run_with_env, write_fixture};

fn run(name: &str, source: &str, env: &[(&str, &str)]) -> Vec<String> {
    let crate_name = format!("w3cos_node_io_{name}");
    let root = write_fixture(
        &crate_name,
        &[
            ("package.json", r#"{"name":"node-io"}"#),
            ("main.js", source),
        ],
    );
    let lines = compile_and_run_with_env(&root, &root.join("main.js"), &crate_name, env);
    std::fs::remove_dir_all(root).ok();
    lines
}

const CHILD_PROCESS_NODE: &str = r#"spawn pid number sh ["sh","-c","echo out1; echo err1 >&2; exit 3"] null false
t1 "out1\n" "err1\n" 3 null
t1 order exit:3:null,close:3:null true
t2 cat 0 "hello world!\n"
t3 error ENOENT -2 spawn definitely-not-a-command-w3cos definitely-not-a-command-w3cos ["x"] spawn definitely-not-a-command-w3cos ENOENT
t3 close -2 null undefined
t4 kill true
t4 exit null SIGTERM true SIGTERM
t5 exec null "SHOUT\n/tmp\n" ""
t5 fail 7 false null "echo bad >&2; exit 7" "Command failed: echo bad >&2; exit 7\nbad\n" "bad\n"
t6 execFile null a-b
t6 promisify "promised\n" ""
t7 spawnSync 2 null in-data "e\n" 3 null number
t7 enc "enc\n" string
t7 enoent ENOENT null 0 null null spawnSync nope-w3cos-cmd ENOENT
t7 execSync "sync\n" s2
t7 execSync fail 4 null "Command failed: exit 4" 0
t7 execFileSync x1y
t7 timeout ETIMEDOUT SIGTERM null
done
"#;

#[test]
fn node_child_process_matches_node() {
    let lines = run(
        "child_process",
        include_str!("fixtures/node_io/child_process.js"),
        &[],
    );
    let expected: Vec<&str> = CHILD_PROCESS_NODE.lines().collect();
    assert_eq!(lines, expected);
}

const NET_NODE: &str = r#"isIP 4 6 0 true true
uds listening true true true
uds connecting true
uds reply echo:HELLO UDS 9 14
uds order listening,connection,client-connect,server-end:hello uds,server-close:false,client-end,client-close:false
uds closed false false
tcp address 127.0.0.1 IPv4 number true
tcp connected 127.0.0.1 true IPv4 number
tcp data 42
tcp connections null 0
tcp server closed
err missing ENOENT connect connect ENOENT <tmp>/w3cos-net-missing.sock
err inuse EADDRINUSE listen true listen EADDRINUSE: address already in use 127.0.0.1:<port>
err refused ECONNREFUSED connect connect ECONNREFUSED 127.0.0.1:<port>
done
"#;

#[test]
fn node_net_matches_node() {
    let lines = run("net", include_str!("fixtures/node_io/net.js"), &[]);
    let expected: Vec<&str> = NET_NODE.lines().collect();
    assert_eq!(lines, expected);
}

const PERMISSION_NODE: &str = r#"has child false has fs.read true
denied ERR_ACCESS_DENIED ChildProcess Access to this API has been restricted
denied ERR_ACCESS_DENIED ChildProcess Access to this API has been restricted
denied ERR_ACCESS_DENIED ChildProcess Access to this API has been restricted
exec denied ERR_ACCESS_DENIED
done
"#;

#[test]
fn node_child_process_permission_matches_node_permission_model() {
    let lines = run(
        "permission",
        include_str!("fixtures/node_io/permission.js"),
        &[("W3COS_NODE_DENY", "child")],
    );
    let expected: Vec<&str> = PERMISSION_NODE.lines().collect();
    assert_eq!(lines, expected);
}

const ES5_SUPER_NODE: &str = r#"explicit 7 1 base:7 true base,own
guarded 8 true true
implicit 3
from set 3 [{"n":1},"two",3]
from map [["k",1],["j",2]] ["k","j"]
duplex finish a+b object
"#;

#[test]
fn classes_extending_es5_constructors_match_node() {
    let lines = run(
        "es5_super",
        include_str!("fixtures/node_io/es5_super.js"),
        &[],
    );
    let expected: Vec<&str> = ES5_SUPER_NODE.lines().collect();
    assert_eq!(lines, expected);
}
