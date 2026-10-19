export function main() {
  return import("./lazy.js").then((mod) => {
    console.log("dyn", mod.answer, mod.bump());
  });
}
