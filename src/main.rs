extern crate pretty_env_logger;

use std::{path::Path, sync::Arc};

use chrono::{DateTime, Local, TimeZone};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::OnceCell;
use tokio_tungstenite::tungstenite::Message;

const KEY_MIRAI_HOST: &str = "mirai_host";
const KEY_MIRAI_VERIFY_KEY: &str = "mirai_verify_key";
const KEY_MIRAI_QQ_STR: &str = "mirai_qq_str";
const KEY_TEST_GROUP_STR: &str = "test_group_str";

const STORE_PATH_PATTERN: &str =
  "store/v2/%Y%m/{group_id}_{sender_id}_{msg_id}_%Y%m%d%H%M%S_{idx}_{size}.{suffix}";

#[derive(Debug)]
struct MiraiImage {
  uuid: String,
  url: String,
}

#[derive(Default, Debug)]
struct BadCqBot {
  host: String,
  verify_key: String,
  qq_num: u64,
  test_group_num: u64,

  http_client: Arc<reqwest::Client>,

  session: Arc<OnceCell<String>>,
}

impl BadCqBot {
  fn new(config: &config::Config) -> Self {
    Self {
      host: config.get_str(KEY_MIRAI_HOST).unwrap(),
      verify_key: config.get_str(KEY_MIRAI_VERIFY_KEY).unwrap(),
      qq_num: config
        .get_str(KEY_MIRAI_QQ_STR)
        .unwrap()
        .parse::<u64>()
        .unwrap(),
      test_group_num: config
        .get_str(KEY_TEST_GROUP_STR)
        .unwrap()
        .parse::<u64>()
        .unwrap(),

      http_client: Arc::new(reqwest::ClientBuilder::new().no_proxy().build().unwrap()),

      ..Default::default()
    }
  }

  async fn get_file_url(self: Arc<Self>, group_id: u64, uuid: &str) -> String {
    let res = self
      .http_client
      .get(format!(
        "http://{}/file/info?sessionKey={}&group={}&id={}&withDownloadInfo=true",
        self.host,
        self.session.get().unwrap(),
        group_id,
        uuid,
      ))
      .send()
      .await
      .unwrap();

    let msg = res.text().await.unwrap();
    let result = json::parse(&msg).unwrap();

    result["data"]["downloadInfo"]["url"]
      .as_str()
      .unwrap()
      .to_string()
  }

  async fn send_friend_message(self: Arc<Self>, target_id: u64, text: &str) {
    let res = self
      .http_client
      .post(format!("http://{}/sendFriendMessage", self.host,))
      .header("content-type", "application/json")
      .body(
        json::object! {
          "sessionKey": self.session.get().unwrap().clone(),
          "target": target_id,
          "messageChain": json::array![
            json::object! {
              "type": "Plain",
              "text": text,
            },
          ],
        }
        .to_string(),
      )
      .send()
      .await
      .unwrap();
  }

  async fn send_group_message(self: Arc<Self>, target_id: u64, text: &str) {
    let res = self
      .http_client
      .post(format!("http://{}/sendGroupMessage", self.host,))
      .header("content-type", "application/json")
      .body(
        json::object! {
          "sessionKey": self.session.get().unwrap().clone(),
          "target": target_id,
          "messageChain": json::array![
            json::object! {
              "type": "Plain",
              "text": text,
            },
          ],
        }
        .to_string(),
      )
      .send()
      .await
      .unwrap();
  }

  // https://github.com/mamoe/mirai/issues/1416
  // receiving files via friend message in backlog
  async fn collect_msg_images(self: Arc<Self>, event: json::JsonValue) {
    let sender_id = &event["data"]["sender"]["id"].as_u64().unwrap();
    let sender_name = &event["data"]["sender"]["nickname"]
      .as_str()
      .unwrap_or_else(|| event["data"]["sender"]["memberName"].as_str().unwrap());

    let group_id = &event["data"]["sender"]["group"]["id"].as_u64();
    let group_name = &event["data"]["sender"]["group"]["name"].as_str();

    let mut msg_id = 0u64;
    let mut sent_ts = Local::now();
    let mut images = Vec::<MiraiImage>::new();

    let msg_chain = &event["data"]["messageChain"];
    for i in 0..msg_chain.len() {
      let msg = &msg_chain[i];
      let msg_type = msg["type"].as_str().unwrap();
      match msg_type {
        "Source" => {
          msg_id = msg["id"].as_u64().unwrap();
          sent_ts = Local.timestamp(msg["time"].as_i64().unwrap(), 0);
        },
        "Image" => {
          // only work for specified groups
          if let Some(group_id) = group_id {
            if *group_id != self.test_group_num {
              continue;
            }
          }

          images.push(MiraiImage {
            uuid: msg["imageId"].as_str().unwrap().to_string(),
            url: msg["url"].as_str().unwrap().to_string(),
          });
        },
        "FlashImage" => {
          images.push(MiraiImage {
            uuid: msg["imageId"].as_str().unwrap().to_string(),
            url: msg["url"].as_str().unwrap().to_string(),
          });
        },
        "File" => {
          if let Some(group_id) = group_id {
            if *group_id != self.test_group_num {
              continue;
            }

            let uuid = msg["id"].as_str().unwrap().to_string();
            let url = self.clone().get_file_url(*group_id, &uuid).await;

            images.push(MiraiImage { uuid, url });
          } else {
            dbg!("file message unhandled");
          }
        },
        _ => {},
      }
    }

    let count = self
      .store_images(sent_ts, *group_id, *sender_id, msg_id, images)
      .await;

    if let Some(group_id) = group_id {
      if *group_id == self.test_group_num {
        self
          .clone()
          .send_group_message(*group_id, &format!("{}", count))
          .await;
      }
    } else {
      self
        .clone()
        .send_friend_message(*sender_id, &format!("{}", count))
        .await;
    }
  }

  async fn store_images(
    &self,
    sent_ts: DateTime<Local>,
    group_id: Option<u64>,
    sender_id: u64,
    msg_id: u64,
    images: Vec<MiraiImage>,
  ) -> usize {
    let mut count = 0;

    for (idx, image) in images.iter().enumerate() {
      let res = self.http_client.get(&image.url).send().await.unwrap();
      match res.bytes().await {
        Ok(bytes) => match infer::get(&bytes[..]) {
          Some(kind) => {
            let path = sent_ts
              .format(STORE_PATH_PATTERN)
              .to_string()
              .replace("{group_id}", &group_id.unwrap_or(0).to_string())
              .replace("{sender_id}", &sender_id.to_string())
              .replace("{msg_id}", &msg_id.to_string())
              .replace("{idx}", &idx.to_string())
              .replace("{size}", &bytes.len().to_string())
              .replace("{suffix}", kind.extension());

            let path = Path::new(&path);

            if let Err(err) = tokio::fs::create_dir_all(&path.parent().unwrap()).await {
              dbg!(&err);
              continue;
            }

            if let Err(err) = tokio::fs::write(&path, &bytes).await {
              dbg!(&err);
              continue;
            }

            log::info!("store image: {:?} {:?}", &path, bytes.len());

            count += 1;
          },
          None => {
            dbg!("file type invalid");
          },
        },
        Err(err) => {
          dbg!(&err);
        },
      }
    }

    count
  }

  async fn run(self: Arc<Self>) {
    loop {
      log::info!("websocket connecting");

      match tokio_tungstenite::connect_async(format!(
        "ws://{}/all?verifyKey={}&qq={}",
        &self.host, &self.verify_key, &self.qq_num,
      ))
      .await
      {
        Ok((stream, res)) => {
          log::info!("websocket connected");

          let (mut writer, mut reader) = stream.split();

          loop {
            tokio::select! {
              Some(msg) = reader.next() => {
                match msg {
                  Ok(msg) => {
                    match msg {
                      Message::Text(msg) => {
                        let event = json::parse(&msg).unwrap();

                        if let Some(data_type) = event["data"]["type"].as_str() {
                          match data_type {
                            "FriendMessage" => {
                              tokio::task::spawn(self.clone().collect_msg_images(event));
                            },
                            "GroupMessage" => {
                              tokio::task::spawn(self.clone().collect_msg_images(event));
                            },
                            "FriendInputStatusChangedEvent" => {},
                            "FriendRecallEvent" => {},
                            "GroupRecallEvent" => {},
                            _ => {
                              dbg!(&msg);
                            },
                          }
                        } else if let Some(session) = event["data"]["session"].as_str() {
                          if let Err(err) = self.session.set(session.to_string()) {
                            dbg!(err);
                          }
                        } else {
                          dbg!(&msg);
                        }
                      },
                      _ => {},
                    }
                  },
                  Err(err) => {
                    dbg!(&err);
                  }
                }
              },
              else => {},
            }
          }
        },
        Err(err) => {
          dbg!(&err);
        },
      }

      log::info!("websocket reconnecting");
    }
  }
}

#[tokio::main]
async fn main() {
  // by default only errors are logged
  pretty_env_logger::init();

  let mut config = config::Config::default();
  config
    .set_default(KEY_MIRAI_HOST, "localhost:8080")
    .unwrap();
  config.merge(config::File::with_name("badcqbot")).unwrap();

  let badcqbot = Arc::new(BadCqBot::new(&config));

  badcqbot.run().await;
}
