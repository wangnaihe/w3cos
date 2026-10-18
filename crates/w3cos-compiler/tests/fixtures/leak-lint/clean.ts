import { Disposable } from 'vs/base/common/lifecycle';
import { Emitter } from 'vs/base/common/event';

export class Clean extends Disposable {
	private readonly _onDidChange = this._register(new Emitter<void>());
	private readonly names = new Map<string, number>();
	private readonly seen = new WeakMap<object, boolean>();

	constructor(model: IModel) {
		super();
		this._register(model.onDidChangeContent(() => this.update()));
	}

	update(): void {
		this.names.set('a', 1);
		this._onDidChange.fire();
	}
}
