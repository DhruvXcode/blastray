export interface Store {
  save(): void;
}

export class BaseStore {}

export class ConcreteStore extends BaseStore implements Store {
  save(): void {}
}

export class SpecializedStore extends ConcreteStore {}
