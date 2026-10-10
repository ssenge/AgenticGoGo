// Preloaded into every node tool of the image (NODE_OPTIONS): node's os.userInfo() throws for a uid
// with no passwd entry — and agg runs the container as the project's owner, whose uid an image
// cannot know (likec4 calls it at start-up). Fall back to what the process and its env say.
'use strict';
const os = require('os');
const real = os.userInfo;
os.userInfo = function userInfo(options) {
  try {
    return real.call(os, options);
  } catch (e) {
    const uid = process.getuid ? process.getuid() : -1;
    const gid = process.getgid ? process.getgid() : -1;
    return { uid, gid, username: process.env.USER || `uid${uid}`, homedir: process.env.HOME || os.tmpdir(), shell: null };
  }
};
require('module').syncBuiltinESMExports();
