import { Disposable } from 'vs/base/common/lifecycle';

export class Widget extends Disposable {
	constructor(private readonly model: IModel, private readonly el: HTMLElement) {
		super();
		model.onDidChangeContent(() => this.render());
		model.onDidDispose(function () { console.log('disposed'); });
		this._register(model.onWillSave(() => this.save()));
		el.addEventListener('click', () => this.render());
		el.addEventListener('keydown', this.onKey);
		el.addEventListener('focus', () => this.render(), { once: true });
		model.onDidChangeLanguage(() => this.render(), this, this.disposables);
	}

	private readonly disposables: IDisposable[] = [];
	private onKey = (e: KeyboardEvent) => { };

	render(): void { }
	save(): void { }

	override dispose(): void {
		this.el.removeEventListener('keydown', this.onKey);
		super.dispose();
	}
}
