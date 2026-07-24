export const frame: () => number;

declare const harmonyRuntime: {
  frame: typeof frame;
};

export default harmonyRuntime;
