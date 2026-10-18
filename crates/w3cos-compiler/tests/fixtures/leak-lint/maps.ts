interface IEditor { id: string }

export class Registry {
	private readonly cache = new Map<IEditor, string>();
	private readonly all: Map<IEditor, number> = new Map();
	private readonly byId = new Map<string, IEditor>();
	private readonly tracked = new Map<IEditor, number>();

	lookup(editor: IEditor): string | undefined {
		if (!this.cache.has(editor)) {
			this.cache.set(editor, editor.id);
		}
		return this.cache.get(editor);
	}

	count(): number {
		let n = 0;
		for (const [, v] of this.all) { n += v; }
		this.byId.set('x', { id: 'x' });
		this.tracked.delete({ id: 'y' });
		return n + this.tracked.size;
	}
}

const views = new Map<HTMLElement, number>();
export function viewIndex(el: HTMLElement): number | undefined {
	return views.get(el);
}
