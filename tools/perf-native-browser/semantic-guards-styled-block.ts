export {};
const TOKEN = { styledBlockToken: true };
function row(name, run) {
  const selected = window.W3COS_REVIEW_CASE;
  if (selected && selected !== name) return;
  let actual;
  try { actual = { value: run() }; }
  catch (e) { actual = { exception: e.name || typeof e, token: e === TOKEN }; }
  console.log("W3COS_PR55_REVIEW " + JSON.stringify({ name: name, actual: actual }));
}
function root() { return document.createElement("div"); }
function fill(doc, parent, n) {
  for (var i = 0; i < n; i++) {
    var el = doc.createElement("div");
    el.textContent = "m " + (i % 50);
    el.style.height = "18px";
    parent.appendChild(el);
  }
}
function textFill(doc, parent, value, n) {
  for (var i = 0; i < n; i++) {
    var el = doc.createElement("div");
    el.textContent = "m " + value;
    el.style.height = "18px";
    parent.appendChild(el);
  }
}
function pristine() {
  const r = root(); fill(document, r, 2);
  return [r.childNodes.length, r.firstChild.textContent, r.lastChild.textContent,
    r.firstChild.style.height];
}
function createOwn() {
  const r = root(), original = document.createElement; let calls = 0;
  document.createElement = function(tag) { calls++; return original.call(this, tag); };
  fill(document, r, 2); return [calls, r.childNodes.length];
}
function createGetter() {
  const r = root(), original = document.createElement; let reads = 0;
  Object.defineProperty(document, "createElement", { configurable: true,
    get: function() { reads++; return original; } });
  fill(document, r, 2); return [reads, r.childNodes.length];
}
function createGetterThrow() {
  const r = root(); Object.defineProperty(document, "createElement", {
    configurable: true, get: function() { throw TOKEN; } });
  fill(document, r, 2); return "getter-was-skipped";
}
function appendOwn() {
  const r = root(); let calls = 0;
  r.appendChild = function(el) { calls++; return el; };
  fill(document, r, 2); return [calls, r.childNodes.length];
}
function appendGetterThrow() {
  const r = root(); Object.defineProperty(r, "appendChild", {
    configurable: true, get: function() { throw TOKEN; } });
  fill(document, r, 2); return "getter-was-skipped";
}
function textSetter() {
  const r = root(); let calls = 0;
  Object.defineProperty(Node.prototype, "textContent", { configurable: true,
    set: function(value) { calls++; this.textSide = value; } });
  fill(document, r, 2); return [calls, r.childNodes.length, r.firstChild.textSide];
}
function textSetterThrow() {
  const r = root(); Object.defineProperty(Node.prototype, "textContent", {
    configurable: true, set: function() { throw TOKEN; } });
  fill(document, r, 2); return "setter-was-skipped";
}
function styleGetter() {
  const r = root(), shadow = { height: "initial" }; let reads = 0;
  Object.defineProperty(HTMLDivElement.prototype, "style", { configurable: true,
    get: function() { reads++; return shadow; } });
  fill(document, r, 2); return [reads, shadow.height, r.childNodes.length];
}
function styleGetterThrow() {
  const r = root(); Object.defineProperty(HTMLDivElement.prototype, "style", {
    configurable: true, get: function() { throw TOKEN; } });
  fill(document, r, 2); return "getter-was-skipped";
}
function styleGetterUndefined() {
  const r = root(); Object.defineProperty(HTMLDivElement.prototype, "style", {
    configurable: true, get: function() { return undefined; } });
  fill(document, r, 2); return "undefined-was-ignored";
}
function styleHeightSetterThrow() {
  const r = root(); Object.defineProperty(CSSStyleDeclaration.prototype, "height", {
    configurable: true, set: function() { throw TOKEN; } });
  fill(document, r, 2); return "setter-was-skipped";
}
function styleHeightSetterReceiver() {
  const r = root(); let calls = 0;
  Object.defineProperty(CSSStyleDeclaration.prototype, "height", { configurable: true,
    set: function() { calls++; this.setProperty("width", "7px"); } });
  fill(document, r, 2); return [calls, r.firstChild.style.width, r.lastChild.style.width];
}
function zeroTripTDZ() {
  function zero() {
    for (var i = 0; i < 0; i++) {
      var el = doc.createElement("div");
      el.textContent = "m " + (i % 50);
      el.style.height = "18px";
      parent.appendChild(el);
    }
    return "ok";
  }
  const result = zero(); let doc = document; let parent = root(); return result;
}
function zeroTripPropertyGetter() {
  const r = root(); let reads = 0;
  Object.defineProperty(document, "createElement", { configurable: true,
    get: function() { reads++; throw TOKEN; } });
  fill(document, r, 0); return [reads, r.childNodes.length];
}
function textFallbackOnce() {
  const r = root(); let conversions = 0;
  const observer = new MutationObserver(function() {});
  observer.observe(r, { childList: true });
  const value = { toString: function() { conversions++; return String(conversions); } };
  textFill(document, r, value, 2); observer.disconnect();
  return [conversions, r.firstChild.textContent, r.lastChild.textContent];
}
function textCreateOrder() {
  const r = root(), original = document.createElement, events = [];
  document.createElement = function(tag) { events.push("create"); return original.call(this, tag); };
  const value = { toString: function() { events.push("value"); return "7"; } };
  textFill(document, r, value, 2); return [events.join(","), r.childNodes.length];
}
function parentRebind() {
  const first = root(), second = root();
  function move(doc, parent, other, n) {
    for (var i = 0; i < n; i++) {
      var el = doc.createElement("div");
      el.textContent = "m " + (i % 50);
      el.style.height = "18px";
      parent.appendChild(el);
      parent = other;
    }
  }
  move(document, first, second, 2); return [first.childNodes.length, second.childNodes.length];
}
row("styled-pristine", pristine);
row("styled-create-own", createOwn);
row("styled-create-getter", createGetter);
row("styled-create-getter-throw", createGetterThrow);
row("styled-append-own", appendOwn);
row("styled-append-getter-throw", appendGetterThrow);
row("styled-text-setter", textSetter);
row("styled-text-setter-throw", textSetterThrow);
row("styled-style-getter", styleGetter);
row("styled-style-getter-throw", styleGetterThrow);
row("styled-style-getter-undefined", styleGetterUndefined);
row("styled-style-height-setter-throw", styleHeightSetterThrow);
row("styled-style-height-setter-receiver", styleHeightSetterReceiver);
row("styled-zero-trip-TDZ", zeroTripTDZ);
row("styled-zero-trip-property-getter", zeroTripPropertyGetter);
row("styled-text-fallback-once", textFallbackOnce);
row("styled-text-create-order", textCreateOrder);
row("styled-parent-rebind", parentRebind);
console.log("W3COS_PR55_REVIEW_DONE");
