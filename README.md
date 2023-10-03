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

## concepts

Ring lets you manage "deployments". 
A deployment is a description of a service requirement

In other words Ring tries to launch services from the requirements
If ring fails it will regularly retry.


## usage 

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

With httpie 

```http POST localhost:3030/deployments bearer -A bearer -a <your_token> < examples/nginx.json``` 

4. Display deployments

```cargo run deployment:list```

or

```ring deployment:list```

5. Inspect deployment

```cargo run deployment:inspect <deployment_id>```


```ring deployment:inspect <deployment_id>```