# Ring

the simple container orchestrator because K8S is   overgineered and docker swarm is dead

## features 

- http API
- container restarting
- container replicating

No load balancing, it is not the goal

## Installation 

### requirements

- rust (to build)
- openssl-sys v0.9.90 (sudo apt install librust-openssl-sys-dev)

run 

```cargo build```


## usage 

1. Init the setup

```cargo run init```

or 

```ring init```

2. Start deamon

```cargo run ring server:start```

or

```ring server:start```

2. Login


```ring login --username admin --password changeme```

or 

```cargo run ring login --username admin --password changeme```


3. Deploy containers (using yaml)

```cargo run ring apply -f ring.yaml```

or 

```ring apply -f examples/ring.yaml```

3. Using http endpoint

With httpie 

```http bearer -A bearer -a <your_token> POST localhost:3030/deployments < examples/nginx.json``` 