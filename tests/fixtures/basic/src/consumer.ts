import createUser, { saveUser as save } from "./imported";

export function useImported() {
    save();
    createUser();
}
