export function leaf() {}

export function middle() {
    leaf();
}

export function entry() {
    middle();
}

export function methodCaller() {
    service.save();
}

export class Worker {
    run() {
        leaf();
    }
}

export class Storage {
    save() {}
}
