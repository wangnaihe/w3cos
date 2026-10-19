class Tagged {
  constructor(n) { this.n = n; }
}
((Tagged2) => {
  Tagged2.extra = 7;
})(Tagged || (Tagged = {}));
export function main() {
  console.log("ns", new Tagged(2).n, Tagged.extra);
}
