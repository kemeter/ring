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
`cargo build`


### usage 

`cargo run init`
or 

`ring init` 
1. run orchestror deamon

`cargo run ring server:start`

or 

`./ring server:start`

2. login

mkdir -p ~/.config/kemeter/ring
echo '{}' >> ~/.config/kemeter/ring

ring login 

3. Deploy containers

Using 
apply with yaml

create yaml

```yaml

```

Using http endpoint
