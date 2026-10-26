//! `child_process`, `net` and `worker_threads` from `w3cos-node` (M3 item 19): each
//! fixture under `fixtures/node_io/` is AOT-compiled and must print what
//! Node 20.19 prints for the same file (`node fixtures/node_io/<name>.js`;
//! the permission fixture under `node --experimental-permission
//! --allow-fs-read=*`, which w3cos matches with `W3COS_NODE_DENY=child`).

use crate::cjs_e2e_tests::{compile_and_run_with_env, write_fixture};

fn run(name: &str, source: &str, env: &[(&str, &str)]) -> Vec<String> {
    run_files(name, &[("main.js", source)], env)
}

fn run_files(name: &str, files: &[(&str, &str)], env: &[(&str, &str)]) -> Vec<String> {
    let crate_name = format!("w3cos_node_io_{name}");
    let mut all = vec![("package.json", r#"{"name":"node-io"}"#)];
    all.extend_from_slice(files);
    let root = write_fixture(&crate_name, &all);
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

const WORKER_THREADS_NODE: &str = r#"main true 0 null null
t1 ready false number true [6,[1,2],1000,1]
t1 square 49
t1 clone [1,"two"] bigint true true undefined true true -Infinity
t1 clone2 true false hi true 1.5,-2 true bad true a+b gi true {"deep":[[1],{"x":null}]}
t1 exit 3 online,ready,square,clone -1
t2 error true TypeError boom from worker
t2 exit 1
t3 first busy
t3 exit 1
t3 terminate 1
t4 receive {"message":{"a":1}} undefined
t4 message ["hello",1]
t4 closed
t5 message from self self true
t5 exit 0
t6 path ERR_WORKER_PATH TypeError
t7 message string 7 undefined
t7 default undefined undefined inherited undefined
done
"#;

#[test]
fn node_worker_threads_matches_node() {
    let lines = run_files(
        "worker_threads",
        &[
            (
                "main.js",
                include_str!("fixtures/node_io/worker_threads/main.js"),
            ),
            (
                "worker.js",
                include_str!("fixtures/node_io/worker_threads/worker.js"),
            ),
        ],
        &[],
    );
    let expected: Vec<&str> = WORKER_THREADS_NODE.lines().collect();
    assert_eq!(lines, expected);
}

const WORKER_PERMISSION_NODE: &str = r#"has worker false
denied ERR_ACCESS_DENIED WorkerThreads Access to this API has been restricted
done
"#;

/// `node --experimental-permission --allow-fs-read=*` (no `--allow-worker`)
/// vs `W3COS_NODE_DENY=worker`.
#[test]
fn node_worker_permission_matches_node_permission_model() {
    let lines = run_files(
        "worker_permission",
        &[
            (
                "main.js",
                include_str!("fixtures/node_io/worker_permission/main.js"),
            ),
            (
                "worker.js",
                include_str!("fixtures/node_io/worker_permission/worker.js"),
            ),
        ],
        &[("W3COS_NODE_DENY", "worker")],
    );
    let expected: Vec<&str> = WORKER_PERMISSION_NODE.lines().collect();
    assert_eq!(lines, expected);
}
