# Ring

A simple container orchestrator with declarative service deployment using containers 

Because K8S is bloated and docker swarm is dead.

## Features 

Ring let you specify "deployments" with service requirements

Ring will compute the diff between what you need and what is already deployed (a la terraform).
Then it will try to close the gap and retry upon failures

- http API
- container restarting
- container replication
- docker engine backend

No load balancing, it is not the goal

## Installation 

### requirements

- rust (to build)
- openssl-sys v0.9.90 (sudo apt install librust-openssl-sys-dev)

```cargo build```

## Usage 

1. Init the setup

```cargo run init```

or 

```ring init```

2. Start deamon

```cargo run server:start```

or

```ring server:start```

2. Login

```ring login --username admin --password changeme```

or 

```cargo run login --username admin --password changeme```

3. Launch deployment (using yaml)

```cargo run apply -f examples/nginx.yaml```

or 

```ring apply -f examples/nginx.yaml```

3. Launch deployment Using http endpoint

Get your token at '~/.config/kemeter/ring'

```http POST localhost:3030/deployments bearer -A bearer -a <your_token> < examples/nginx.json``` 


4. Display deployments

```cargo run deployment:list```

or

```ring deployment:list```

5. Inspect deployment

```cargo run deployment:inspect <deployment_id>```

or

```ring deployment:inspect <deployment_id>```