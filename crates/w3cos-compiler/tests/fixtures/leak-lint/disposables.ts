import { Disposable, DisposableStore } from 'vs/base/common/lifecycle';
import { Emitter } from 'vs/base/common/event';

export class Model extends Disposable {
	private readonly _onDidChange = new Emitter<void>();
	readonly onDidChange = this._onDidChange.event;
	private readonly _onDidSave = this._register(new Emitter<void>());
	private readonly store: DisposableStore;

	constructor() {
		super();
		this.store = new DisposableStore();
	}
}

export class Session {
	private readonly _onDidEnd = new Emitter<void>();
	private readonly _onDidStart = new Emitter<void>();

	dispose(): void {
		this._onDidEnd.dispose();
	}
}
