package example;

interface Store {}

class BaseStore {}

class ConcreteStore extends BaseStore implements Store {}

class SpecializedStore extends ConcreteStore {}
