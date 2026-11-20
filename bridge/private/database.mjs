import { Client } from '@neondatabase/serverless';

// One client per Worker request; never share a transaction-bearing client across requests.
export async function openDatabase(url) {
  const client = new Client(url);
  await client.connect();
  return {
    query: (sql, args) => client.query(sql, args),
    transaction: async fn => {
      await client.query('BEGIN');
      try { const value = await fn(client); await client.query('COMMIT'); return value; }
      catch (e) { await client.query('ROLLBACK'); throw e; }
    },
    close: () => client.end(),
  };
}
