# Swarmdon

Sync [Swarm](https://swarmapp.com/) checkins to Mastodon.

## Setup

### Prerequisite

- Create a Foursquare application in [Foursquare Developer Console](https://foursquare.com/developers/home).
- In Push API
  - Push Notifications: select "Push checkins by this project's users"
  - Push URL: fill your deployment URL with `/swarm/push` (e.g. `https://your-app-here.example.com/swarm/push`)
  - Push Version: 20230621
- Grab Client ID, Client Secret and Push Secret from OAuth Authentication section

### Run

```
docker build -t swarmdon
docker run -p 8000:8000 -v $PWD/swarmdon.db:/swarmdon.db swarmdon --address 0.0.0.0:8000 --base-url <BASE_URL> --swarm-client-id <CLIENT_ID> --swarm-client-secret <CLIENT_SECRET> --swarm-push-secret <PUSH_SECRET>
```

Enjoy!

I am not committed to keep developing this small app. If you want any features beyond mere syncing, feel free to send a PR.

## License

MIT or Apache 2.0
