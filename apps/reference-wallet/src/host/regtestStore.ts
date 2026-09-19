/**
 * Regtest-only browser persistence for bearer proofs and in-flight melts.
 * This is deliberately unavailable outside a browser and is never used by
 * live mode. Proof material must remain in the host, not EcashMesh.
 */
export type StoredProof = {
  amount: string;
  id: string;
  secret: string;
  C: string;
  witness?: string;
};

export type PendingMelt = {
  paymentId: string;
  mintUrl: string;
  quote: string;
  inputs: StoredProof[];
  retained: StoredProof[];
  outputData: unknown[];
  createdAt: number;
};

const databaseName = "ecashmesh-regtest-custody";
const databaseVersion = 1;
const proofsStore = "proofs";
const pendingStore = "pending-melts";

export class RegtestStore {
  static async open() {
    if (typeof indexedDB === "undefined") {
      throw new Error("Regtest custody requires a browser with IndexedDB.");
    }
    const database = await new Promise<IDBDatabase>((resolve, reject) => {
      const request = indexedDB.open(databaseName, databaseVersion);
      request.onerror = () => reject(request.error);
      request.onupgradeneeded = () => {
        const db = request.result;
        if (!db.objectStoreNames.contains(proofsStore))
          db.createObjectStore(proofsStore);
        if (!db.objectStoreNames.contains(pendingStore))
          db.createObjectStore(pendingStore);
      };
      request.onsuccess = () => resolve(request.result);
    });
    return new RegtestStore(database);
  }

  private constructor(private readonly database: IDBDatabase) {}

  async proofs(mintUrl: string): Promise<StoredProof[]> {
    return this.read<StoredProof[]>(proofsStore, mintUrl).then(
      (value) => value ?? [],
    );
  }

  async replaceProofs(mintUrl: string, proofs: StoredProof[]) {
    await this.write(proofsStore, mintUrl, proofs);
  }

  async savePending(record: PendingMelt) {
    await this.write(pendingStore, record.paymentId, record);
  }

  async pending(paymentId: string): Promise<PendingMelt | undefined> {
    return this.read<PendingMelt>(pendingStore, paymentId);
  }

  async clearPending(paymentId: string) {
    await this.remove(pendingStore, paymentId);
  }

  private read<T>(store: string, key: string): Promise<T | undefined> {
    return new Promise((resolve, reject) => {
      const request = this.database
        .transaction(store)
        .objectStore(store)
        .get(key);
      request.onerror = () => reject(request.error);
      request.onsuccess = () => resolve(request.result as T | undefined);
    });
  }

  private write(store: string, key: string, value: unknown): Promise<void> {
    return this.transaction(store, "readwrite", (objectStore) =>
      objectStore.put(value, key),
    );
  }

  private remove(store: string, key: string): Promise<void> {
    return this.transaction(store, "readwrite", (objectStore) =>
      objectStore.delete(key),
    );
  }

  private transaction(
    store: string,
    mode: IDBTransactionMode,
    action: (objectStore: IDBObjectStore) => IDBRequest,
  ): Promise<void> {
    return new Promise((resolve, reject) => {
      const transaction = this.database.transaction(store, mode);
      transaction.onerror = () => reject(transaction.error);
      transaction.oncomplete = () => resolve();
      action(transaction.objectStore(store));
    });
  }
}
