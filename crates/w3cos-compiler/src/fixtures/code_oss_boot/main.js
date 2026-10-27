// Code-OSS bootstrap patterns (M3 item 23): createRequire, node:module
// hooks, and the builtins the Electron main graph loads.
import { createRequire, isBuiltin, registerHooks } from "node:module";
import nodeModule from "node:module";
import * as v8 from "node:v8";
import { performance, PerformanceObserver, monitorEventLoopDelay } from "node:perf_hooks";
import { AsyncLocalStorage, AsyncResource } from "node:async_hooks";
import * as dc from "node:diagnostics_channel";
import * as readline from "node:readline";
import { PassThrough } from "node:stream";
import * as dns from "node:dns";
import * as http from "node:http";
import * as https from "node:https";
import * as tls from "node:tls";
import * as tty from "node:tty";
import { Console } from "node:console";

const require = createRequire(import.meta.url);
const data = require("./data.json");
console.log("json", data.nameShort, data.list.length, require("./data.json") === data);
const helper = require("./helper.cjs");
console.log("cjs", helper.loaded, helper.kind, require("./helper.cjs") === helper);
let missing;
try { require("./product.overrides.json"); } catch (error) { missing = error.code; }
console.log("missing", missing);
const Module = require("node:module");
console.log("module", typeof Module, typeof Module._resolveLookupPaths, Array.isArray(Module.globalPaths), Module.createRequire === createRequire);
const original = Module._resolveLookupPaths;
Module._resolveLookupPaths = function (request, parent) { return original(request, parent); };
console.log("lookup", typeof Module._resolveLookupPaths, Array.isArray(Module._nodeModulePaths("/a/b")), Module._nodeModulePaths("/a/b")[0]);
console.log("builtin", isBuiltin("fs"), isBuiltin("node:fs"), isBuiltin("node:sqlite"), isBuiltin("sqlite"), isBuiltin("electron"), isBuiltin("fs/promises"));
const hooks = registerHooks({ resolve(specifier, context, next) { return next(specifier, context); } });
console.log("hooks", typeof hooks.deregister);
const fs = require("fs");
console.log("require builtin", typeof fs.readFileSync, require("node:path").join("a", "b"));
// extHostExtensionService: `nodeModule.createRequire(...)` on a default import.
const require2 = nodeModule.createRequire(import.meta.url);
console.log("nsreq", typeof require2("module")._nodeModulePaths, require2("./data.json") === data);

const heap = v8.getHeapStatistics();
console.log("v8", typeof heap.used_heap_size, typeof v8.getHeapCodeStatistics().code_and_metadata_size, typeof v8.setFlagsFromString, v8.cachedDataVersionTag() >= 0);
console.log("perf", typeof performance.now(), typeof PerformanceObserver, typeof monitorEventLoopDelay().enable);

const als = new AsyncLocalStorage();
console.log("als", als.getStore(), als.run({ id: 7 }, () => als.getStore().id), als.getStore());
const resource = new AsyncResource("job");
console.log("resource", resource.runInAsyncScope((a, b) => a + b, null, 2, 3), typeof resource.asyncId());

const channel = dc.channel("w3cos:test");
const seen = [];
const listener = (message, name) => seen.push(name + ":" + message.n);
console.log("dc", channel.hasSubscribers, dc.hasSubscribers("w3cos:test"));
channel.subscribe(listener);
channel.publish({ n: 1 });
console.log("dc2", channel.hasSubscribers, seen.join(), channel.unsubscribe(listener), channel.unsubscribe(listener), dc.channel("w3cos:test") === channel);

console.log("http", http.STATUS_CODES[404], http.METHODS.includes("PATCH"), typeof http.Agent, new http.Agent({ keepAlive: true }).keepAlive, new https.Agent().defaultPort, https.globalAgent.protocol);
console.log("tls", tls.DEFAULT_MIN_VERSION, tls.DEFAULT_ECDH_CURVE, typeof tls.connect, typeof tls.TLSSocket);
console.log("tty", tty.isatty(99), typeof tty.WriteStream);
console.log("console", typeof Console);

// readline runs after the lookups so the line order is deterministic.
dns.lookup("127.0.0.1", (error, address, family) => {
  console.log("dns", error, address, family);
  dns.lookup("localhost", { family: 4 }, (error2, address2) => {
    console.log("dns2", error2, address2);
    readLines();
  });
});

function readLines() {
  const input = new PassThrough();
  const rl = readline.createInterface({ input });
  const lines = [];
  rl.on("line", (line) => lines.push(line));
  rl.on("close", () => console.log("readline", JSON.stringify(lines)));
  input.write("alpha\nbe");
  input.write("ta\r\ngamma");
  input.end();
}
