export function cycleA() {
    cycleB();
}

export function cycleB() {
    cycleA();
}
