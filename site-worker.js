// Keep old links, bookmarks and shared videos working after the rename.
export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (["agent-view.app", "www.agent-view.app", "www.betteragentview.com"].includes(url.hostname)) {
      url.protocol = "https:";
      url.hostname = "betteragentview.com";
      url.port = "";
      return Response.redirect(url.toString(), 308);
    }
    return env.ASSETS.fetch(request);
  },
};
