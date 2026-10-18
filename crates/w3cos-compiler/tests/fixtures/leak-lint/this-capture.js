export class Button {
	constructor(el) {
		this.el = el;
		this.handler = this.onClick.bind(this);
		this.fn = () => this.el;
	}
	onClick() { }
}

export function install() {
	const obj = { x: 1 };
	obj.fn = () => obj.x;
	return obj;
}

export function link() {
	const a = {};
	const b = {};
	a.b = b;
	b.a = a;
	return a;
}
