export class TreeNode {
	parent: TreeNode | undefined;
	readonly children: TreeNode[] = [];

	add(child: TreeNode): void {
		child.parent = this;
		this.children.push(child);
	}
}

export class Panel {
	private owner: Editor;
	protected readonly host: Workbench;

	constructor(owner: Editor, host: Workbench) {
		this.owner = owner;
		this.host = host;
	}
}

export class Toolbar {
	constructor(private readonly container: Workbench) { }
}

export class Counter {
	parent: number = 0;
	bump(): void { this.parent = 1; }
}

export class Overlay {
	private container: HTMLElement | undefined;
	private parentBounds: IBounds;
	show(container: HTMLElement, bounds: IBounds): void {
		this.container = container;
		this.parentBounds = bounds;
	}
}
