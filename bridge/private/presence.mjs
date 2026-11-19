/** Timestamps only. Never reads the outbox: a queued message is not a Mac. */
export const IDLE_SYNC_MS=15*60_000;
export const HEARTBEAT_ONLINE_MS=90_000;
export const HISTORY_FRESH_MS=17*60_000;

export function macPresence(now,stamps={}) {
  const last_heartbeat_at=finite(stamps.last_seen);
  const last_successful_sync_at=finite(stamps.checked);
  const last_attempt_at=finite(stamps.sync_attempted);
  const last_runtime_at=finite(stamps.runtime_checked);
  const contacts=[last_heartbeat_at,last_attempt_at,last_runtime_at].filter(v=>v!==null);
  const last_contact_at=contacts.length?Math.max(...contacts):null;
  const heartbeat_age_ms=last_heartbeat_at===null?null:Math.max(0,now-last_heartbeat_at);
  const sync_age_ms=last_successful_sync_at===null?null:Math.max(0,now-last_successful_sync_at);
  const bridge=last_heartbeat_at===null?'unknown':heartbeat_age_ms<=HEARTBEAT_ONLINE_MS?'online':'offline';
  // A recent connector HTTPS call proves this worker was reachable. Silence can be
  // sleep, a stopped daemon, or a network loss, so it is unknown, not "no internet".
  const internet=bridge==='online'?'reachable':'unknown';
  const skipped=Number.isInteger(stamps.sync_skipped)&&stamps.sync_skipped>0?stamps.sync_skipped:0;
  let history_sync='unknown';
  if(last_successful_sync_at!==null) {
    const partial=skipped>0&&last_attempt_at!==null&&last_attempt_at>last_successful_sync_at;
    history_sync=partial?'partial':sync_age_ms<=HISTORY_FRESH_MS?'fresh':'stale';
  }
  return {
    bridge,internet,history_sync,last_heartbeat_at,last_successful_sync_at,last_contact_at,
    heartbeat_age_ms,sync_age_ms,stale:history_sync!=='fresh',sync_skipped:skipped,
    queue_acceptance_means_mac_online:false,
    cadence:{
      idle_history_sync_seconds:IDLE_SYNC_MS/1000,
      active_tick_seconds:5,
      active_window_seconds:300,
      poll_wait_seconds:20,
      explanation:'Idle history sync is every 15 minutes so the database can suspend. Five seconds is only the loop tick while a phone request is active, not the idle interval.',
    },
  };
}
function finite(value){return Number.isFinite(value)?value:null;}
