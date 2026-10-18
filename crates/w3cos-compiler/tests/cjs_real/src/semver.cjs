const semver = require("semver");
console.log(semver.valid("1.2.3"), semver.valid("a.b.c"), semver.clean("  =v1.2.3   "));
console.log(semver.satisfies("1.2.3", "1.x || >=2.5.0 || 5.0.0 - 7.2.3"), semver.gt("1.2.3", "9.8.7"), semver.lt("1.2.3", "9.8.7"));
console.log(semver.minVersion(">=1.0.0").version, semver.inc("1.2.3", "prerelease", "beta"), semver.coerce("v2").version);
console.log(JSON.stringify(semver.sort(["1.10.0", "1.2.0", "1.2.0-rc.1", "0.9.9"])));
const range = new semver.Range("^1.2.3 || ~2.0.0");
console.log(range.range, range.test("1.9.0"), range.test("2.1.0"));
const Comparator = require("semver/classes/comparator");
console.log(new Comparator(">=1.2.3").test("1.3.0"), semver.intersects("^1.0.0", "1.5.x"));
