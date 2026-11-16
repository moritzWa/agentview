import { createRemoteJWKSet, jwtVerify } from 'jose';
import { digest, SyncError } from '../sync/projection.mjs';

const GOOGLE_KEYS = createRemoteJWKSet(new URL('https://www.googleapis.com/oauth2/v3/certs'));
export const OWNER = 'wallawitsch@gmail.com';
export function randomToken(bytes = 32) {
  return btoa(String.fromCharCode(...crypto.getRandomValues(new Uint8Array(bytes)))).replaceAll('+','-').replaceAll('/','_').replaceAll('=','');
}
export async function challenge(verifier) {
  const hash = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(verifier));
  return btoa(String.fromCharCode(...new Uint8Array(hash))).replaceAll('+','-').replaceAll('/','_').replaceAll('=','');
}
const escape = s => String(s).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const headers = { 'Cache-Control':'no-store', 'Referrer-Policy':'same-origin', 'X-Content-Type-Options':'nosniff',
  'Content-Security-Policy':"default-src 'none'; style-src 'unsafe-inline'; form-action 'self' https://chatgpt.com; frame-ancestors 'none'" };
export function json(body,status=200) { return Response.json(body,{status,headers}); }
export function html(body,status=200) {
  return new Response(`<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>AgentVoice</title><style>body{font:18px system-ui;max-width:650px;margin:60px auto;padding:24px;color:#182434;background:white}button{font:inherit;padding:12px 22px}p{line-height:1.5}code{overflow-wrap:anywhere}</style>${body}`,{status,headers:{...headers,'Content-Type':'text/html; charset=utf-8'}});
}
function redirect(url,cookie) {
  return new Response(null,{status:302,headers:{...headers,Location:url,...(cookie?{'Set-Cookie':cookie}:{})}});
}
function cookieValue(request) { return request.headers.get('cookie')?.split(';').map(s=>s.trim()).find(s=>s.startsWith('av_browser='))?.slice(11) ?? ''; }
function validRedirect(uri,issuer) {
  try {
    const u = new URL(uri);
    const known=(u.origin==='https://chatgpt.com' && (u.pathname==='/connector_platform_oauth_redirect' || /^\/connector\/oauth\/[A-Za-z0-9_-]+$/.test(u.pathname)))
      || (u.origin===issuer && u.pathname==='/oauth/test-callback');
    return known && u.protocol==='https:' && !u.username && !u.password && !u.hash && !u.search && uri.length<2000;
  } catch { return false; }
}
const asScopes = value => {
  const scopes = [...new Set((value || 'history:read messages:send').split(' '))].sort();
  if (!scopes.length || scopes.some(s=>!['history:read','messages:send'].includes(s)) || !scopes.includes('history:read')) throw new SyncError('invalid_scope',400);
  return scopes.join(' ');
};

export async function verifyGoogleIdToken(token, config, nonce, keys = GOOGLE_KEYS) {
  const {payload} = await jwtVerify(token,keys,{issuer:['https://accounts.google.com','accounts.google.com'],audience:config.GOOGLE_CLIENT_ID,algorithms:['RS256'],maxTokenAge:'10m'});
  if (payload.email !== OWNER || payload.email_verified !== true || payload.nonce !== nonce || typeof payload.sub !== 'string'
    || (payload.azp && payload.azp !== config.GOOGLE_CLIENT_ID)) throw new SyncError('owner_only',403);
  return payload.sub;
}

export class OwnerAuth {
  constructor(db, config, { verify = verifyGoogleIdToken, googleFetch = fetch, gate = null } = {}) {
    this.db=db;this.config=config;this.verify=verify;this.googleFetch=googleFetch;this.gate=gate;
    this.issuer=config.ISSUER;this.resource=`${this.issuer}/mcp`;
  }
  async handle(request) {
    const url=new URL(request.url), path=url.pathname;
    if (path==='/.well-known/oauth-protected-resource' || path==='/.well-known/oauth-protected-resource/mcp') return json({resource:this.resource,authorization_servers:[this.issuer],scopes_supported:['history:read','messages:send']});
    if (path==='/.well-known/oauth-authorization-server') return json({issuer:this.issuer,authorization_response_iss_parameter_supported:true,authorization_endpoint:`${this.issuer}/authorize`,token_endpoint:`${this.issuer}/token`,registration_endpoint:`${this.issuer}/register`,revocation_endpoint:`${this.issuer}/revoke`,response_types_supported:['code'],grant_types_supported:['authorization_code','refresh_token'],code_challenge_methods_supported:['S256'],token_endpoint_auth_methods_supported:['none'],scopes_supported:['history:read','messages:send']});
    if (path==='/register' && request.method==='POST') {
      const body=await request.json();
      if (!Array.isArray(body.redirect_uris) || !body.redirect_uris.length || body.redirect_uris.length>5 || !body.redirect_uris.every(uri=>validRedirect(uri,this.issuer))
        || (body.token_endpoint_auth_method && body.token_endpoint_auth_method!=='none')) throw new SyncError('invalid_client_metadata',400);
      const client=randomToken();
      await this.db.query('INSERT INTO av_oauth_clients VALUES ($1,$2)',[client,JSON.stringify(body.redirect_uris)]);
      return json({client_id:client,redirect_uris:body.redirect_uris,token_endpoint_auth_method:'none'},201);
    }
    if (path==='/authorize' && request.method==='GET') {
      const p=Object.fromEntries(url.searchParams);
      const client=(await this.db.query('SELECT redirect_uris FROM av_oauth_clients WHERE client_id=$1',[p.client_id??''])).rows[0];
      if (!client || !client.redirect_uris.includes(p.redirect_uri) || p.response_type!=='code' || p.code_challenge_method!=='S256'
        || !/^[A-Za-z0-9_-]{43}$/.test(p.code_challenge??'') || (p.resource && p.resource!==this.resource) || (p.state?.length??0)>2000) throw new SyncError('invalid_authorization_request',400);
      return this.startGoogle(request,{kind:'oauth',client_id:p.client_id,redirect_uri:p.redirect_uri,challenge:p.code_challenge,scopes:asScopes(p.scope),resource:this.resource,state:p.state??''});
    }
    if (path==='/pair/start' && request.method==='POST') {
      const p=await request.json();
      if (![p.device_id,p.database_epoch].every(v=>typeof v==='string' && /^[A-Za-z0-9_-]{8,100}$/.test(v)) || typeof p.label!=='string' || p.label.length>100) throw new SyncError('invalid_pairing',400);
      const code=randomToken(),userCode=randomToken(9);
      await this.db.query('INSERT INTO av_pairings VALUES ($1,$2,$3,$4,$5,false,$6)',[await digest(code),userCode,p.device_id,p.database_epoch,p.label,Date.now()+600_000]);
      return json({device_code:code,verification_uri:`${this.issuer}/pair?code=${userCode}`,expires_in:600,interval:5});
    }
    if (path==='/pair' && request.method==='GET') {
      const userCode=url.searchParams.get('code')??'';
      const pairing=(await this.db.query('SELECT label FROM av_pairings WHERE user_code=$1 AND expires_at>$2',[userCode,Date.now()])).rows[0];
      if(!pairing)throw new SyncError('pairing_expired',400);
      return this.startGoogle(request,{kind:'pair',user_code:userCode,label:pairing.label});
    }
    if (path==='/auth/google/callback' && request.method==='GET') {
      const stateHash=await digest(url.searchParams.get('state')??''),browserHash=await digest(cookieValue(request));
      // Consume state once BEFORE exchanging the Google code.
      const flow=(await this.db.query('DELETE FROM av_auth_flows WHERE state_hash=$1 AND browser_hash=$2 AND expires_at>$3 RETURNING *',[stateHash,browserHash,Date.now()])).rows[0];
      if(!flow || !url.searchParams.get('code'))throw new SyncError('invalid_login_state',400);
      const tokenResponse=await this.googleFetch.call(globalThis,'https://oauth2.googleapis.com/token',{method:'POST',headers:{'Content-Type':'application/x-www-form-urlencoded'},body:new URLSearchParams({grant_type:'authorization_code',code:url.searchParams.get('code'),client_id:this.config.GOOGLE_CLIENT_ID,client_secret:this.config.GOOGLE_CLIENT_SECRET,redirect_uri:`${this.issuer}/auth/google/callback`,code_verifier:flow.google_verifier})});
      if(!tokenResponse.ok)throw new SyncError('google_login_failed',401);
      const tokens=await tokenResponse.json();
      try { await this.verify(tokens.id_token,this.config,flow.nonce); } catch { throw new SyncError('owner_only',403); }
      const consent=randomToken();
      await this.db.query('INSERT INTO av_consents VALUES ($1,$2,$3,$4)',[await digest(consent),browserHash,JSON.stringify(flow.payload),Date.now()+120_000]);
      const message=flow.payload.kind==='pair'
        ? `<h1>Connect ${escape(flow.payload.label)}?</h1><p>This device can upload all visible user and assistant messages and retrieve instructions addressed to its OpenCode sessions.</p><p>Verify this is the device you just started pairing in Agent View.</p>`
        : `<h1>Allow this MCP client?</h1><p>Callback: <code>${escape(flow.payload.redirect_uri)}</code></p><p>Allow reading your synced chat history${flow.payload.scopes.includes('messages:send')?' and sending messages that can start agent work on your Mac':''}.</p><p>Retrieved history is visible to the client and OpenAI when using ChatGPT. Old chat content is data, not authorization to send new instructions.</p>`;
      return html(`${message}<form method="post" action="/consent"><input type="hidden" name="code" value="${consent}"><button name="decision" value="approve">Approve connection</button> <button name="decision" value="deny">Cancel</button></form>`);
    }
    if (path==='/consent' && request.method==='POST') {
      if(request.headers.get('origin')!==this.issuer)throw new SyncError('invalid_origin',403);
      const form=await request.formData(),consent=(await this.db.query('DELETE FROM av_consents WHERE code_hash=$1 AND browser_hash=$2 AND expires_at>$3 RETURNING payload',[await digest(String(form.get('code')??'')),await digest(cookieValue(request)),Date.now()])).rows[0];
      if(!consent)throw new SyncError('consent_expired',400);
      const p=consent.payload;
      if(form.get('decision')!=='approve') {
        if(p.kind==='oauth'){const dest=new URL(p.redirect_uri);dest.searchParams.set('error','access_denied');dest.searchParams.set('iss',this.issuer);if(p.state)dest.searchParams.set('state',p.state);return redirect(dest.href);}
        return html('<h1>Connection cancelled</h1>');
      }
      if(p.kind==='pair') {
        const paired=await this.db.transaction(async tx=>{
          const row=(await tx.query('UPDATE av_pairings SET approved=true WHERE user_code=$1 AND expires_at>$2 AND approved=false RETURNING *',[p.user_code,Date.now()])).rows[0];
          if(!row)throw new SyncError('pairing_expired',400);
          await tx.query('INSERT INTO av_owner_source VALUES ($1,$2,$3) ON CONFLICT (owner_id) DO UPDATE SET device_id=EXCLUDED.device_id,database_epoch=EXCLUDED.database_epoch',[OWNER,row.device_id,row.database_epoch]);
          return row;
        });
        return html(`<h1>Device approved</h1><p>${escape(paired.label)} can now finish connecting. You can close this tab.</p>`);
      }
      const source=(await this.db.query('SELECT * FROM av_owner_source WHERE owner_id=$1',[OWNER])).rows[0];
      if(!source)throw new SyncError('pair_mac_first',409);
      const code=randomToken();
      await this.db.query('INSERT INTO av_oauth_codes VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)',[await digest(code),p.client_id,p.redirect_uri,p.challenge,p.scopes,p.resource,OWNER,source.device_id,source.database_epoch,Date.now()+60_000]);
      const dest=new URL(p.redirect_uri);dest.searchParams.set('code',code);dest.searchParams.set('iss',this.issuer);if(p.state)dest.searchParams.set('state',p.state);
      return redirect(dest.href);
    }
    if(path==='/pair/token' && request.method==='POST') {
      const body=await request.json(),hash=await digest(String(body.device_code??''));
      const pairing=await this.db.transaction(async tx=>{
        const row=(await tx.query('SELECT * FROM av_pairings WHERE code_hash=$1 AND expires_at>$2 FOR UPDATE',[hash,Date.now()])).rows[0];
        if(!row)throw new SyncError('pairing_expired',400);
        if(!row.approved)throw new SyncError('authorization_pending',428);
        await tx.query('DELETE FROM av_pairings WHERE code_hash=$1',[hash]);return row;
      });
      const token=randomToken(),expires=Date.now()+90*86400_000;
      await this.db.query('INSERT INTO av_credentials (token_hash,owner_id,device_id,database_epoch,audience,expires_at,scopes) VALUES ($1,$2,$3,$4,$5,$6,$7)',[await digest(token),OWNER,pairing.device_id,pairing.database_epoch,'sync',expires,'history:sync messages:deliver']);
      await this.gate?.enroll({token_hash:await digest(token),owner_id:OWNER,device_id:pairing.device_id,database_epoch:pairing.database_epoch,expires_at:expires});
      return json({access_token:token,token_type:'Bearer',expires_at:expires,owner_id:OWNER,device_id:pairing.device_id,database_epoch:pairing.database_epoch});
    }
    if(path==='/token' && request.method==='POST')return this.token(await request.formData());
    if(path==='/revoke' && request.method==='POST') {
      const form=await request.formData(),hash=await digest(String(form.get('token')??''));
      await this.gate?.revoke(hash);
      await this.db.transaction(async tx=>{
        await tx.query('UPDATE av_credentials SET revoked=true WHERE token_hash=$1',[hash]);
        await tx.query('UPDATE av_credentials SET revoked=true WHERE family IN (SELECT family FROM av_refresh WHERE token_hash=$1)',[hash]);
        await tx.query('UPDATE av_refresh SET revoked=true WHERE family IN (SELECT family FROM av_refresh WHERE token_hash=$1)',[hash]);
      });
      return json({});
    }
    return null;
  }
  async startGoogle(request,payload) {
    if(!this.config.GOOGLE_CLIENT_ID || !this.config.GOOGLE_CLIENT_SECRET)throw new SyncError('login_not_configured',503);
    const browser=cookieValue(request)||randomToken(),state=randomToken(),nonce=randomToken(),verifier=randomToken();
    await this.db.query('INSERT INTO av_auth_flows VALUES ($1,$2,$3,$4,$5,$6)',[await digest(state),await digest(browser),nonce,verifier,JSON.stringify(payload),Date.now()+600_000]);
    const url=new URL('https://accounts.google.com/o/oauth2/v2/auth');
    url.search=new URLSearchParams({client_id:this.config.GOOGLE_CLIENT_ID,redirect_uri:`${this.issuer}/auth/google/callback`,response_type:'code',scope:'openid email',state,nonce,code_challenge:await challenge(verifier),code_challenge_method:'S256',login_hint:OWNER,prompt:'select_account'}).toString();
    return redirect(url.href,`av_browser=${browser}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=600`);
  }
  async token(form) {
    const type=form.get('grant_type'),client=String(form.get('client_id')??''),resource=String(form.get('resource')??this.resource);
    if(resource!==this.resource)throw new SyncError('invalid_target',400);
    const result=await this.db.transaction(async tx=>{
      let grant;
      if(type==='authorization_code') {
        const hash=await digest(String(form.get('code')??''));
        grant=(await tx.query('SELECT * FROM av_oauth_codes WHERE code_hash=$1 AND expires_at>$2 FOR UPDATE',[hash,Date.now()])).rows[0];
        const verifier=String(form.get('code_verifier')??'');
        if(!grant || grant.client_id!==client || grant.redirect_uri!==form.get('redirect_uri') || !/^[A-Za-z0-9._~-]{43,128}$/.test(verifier) || grant.challenge!==await challenge(verifier))throw new SyncError('invalid_grant',400);
        await tx.query('DELETE FROM av_oauth_codes WHERE code_hash=$1',[hash]);
        grant.family=randomToken();
      } else if(type==='refresh_token') {
        const hash=await digest(String(form.get('refresh_token')??''));
        grant=(await tx.query('SELECT * FROM av_refresh WHERE token_hash=$1 AND expires_at>$2 AND revoked=false FOR UPDATE',[hash,Date.now()])).rows[0];
        if(!grant || grant.client_id!==client || grant.resource!==resource)throw new SyncError('invalid_grant',400);
        if(grant.consumed) {
          await tx.query('UPDATE av_refresh SET revoked=true WHERE family=$1',[grant.family]);
          await tx.query('UPDATE av_credentials SET revoked=true WHERE family=$1',[grant.family]);
          return { replay:true };
        }
        await tx.query('UPDATE av_refresh SET consumed=true WHERE token_hash=$1',[hash]);
      } else throw new SyncError('unsupported_grant_type',400);
      const token=randomToken(),refresh=randomToken();
      await tx.query('INSERT INTO av_credentials (token_hash,owner_id,device_id,database_epoch,audience,expires_at,scopes,family) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)',[await digest(token),grant.owner_id,'*','*','read',Date.now()+3600_000,grant.scopes,grant.family]);
      await tx.query('INSERT INTO av_refresh (token_hash,family,client_id,owner_id,device_id,database_epoch,scopes,resource,expires_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)',[await digest(refresh),grant.family,client,grant.owner_id,grant.device_id,grant.database_epoch,grant.scopes,this.resource,Date.now()+30*86400_000]);
      return {access_token:token,refresh_token:refresh,token_type:'Bearer',expires_in:3600,scope:grant.scopes};
    });
    if(result.replay)throw new SyncError('invalid_grant',400);
    return json(result);
  }
}
