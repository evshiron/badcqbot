const { App } = require('koishi');
const fetch = require('node-fetch');

const DAZE_HOST_ADD_URL = 'https://daze-now.herokuapp.com/api/host/add';

const app = new App({
  type: 'http',
  port: 5701,
  server: 'http://localhost:5700',
});

app.receiver.on('message/friend', (meta) => {
  app.sender.sendPrivateMsg(meta.userId, meta.message);
});

app.receiver.on('message/normal', (meta) => {
  (async () => {
    if (meta.message.indexOf('[CQ:at') !== -1) {
      return;
    }

    meta.message = meta.message.replace(/\[CQ[^\]]+\]/g, '');

    const match = meta.message.match(/(\d{1,3})\W{1,3}?(\d{1,3})\W{1,3}?(\d{1,3})\W{1,3}?(\d{1,3})\W{1,3}?(\d{3,5})/);
    if (match) {
      const addr = `${match[1]}.${match[2]}.${match[3]}.${match[4]}:${match[5]}`;
      const data = await fetch(DAZE_HOST_ADD_URL, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({
          addr,
          desc: meta.message,
        }),
      }).then(res => res.json());
    }
  })();
});

app.start();
