THIS_FILE := $(lastword $(MAKEFILE_LIST))

## ==========================================================
##
##                            		888888ba  
##                                  88    `8b 
##    .d8888b. 88d8b.d8b. .d8888b. a88aaaa8P' 
##    88ooood8 88'`88'`88 88'  `88  88   `8b. 
##    88.  ... 88  88  88 88.  .88  88     88 
##    `88888P' dP  dP  dP `8888P88  dP     dP 
##                             .88            
##                         d8888P             
##
##   -------------------------
##   Makefile for the project
##   Author: @stephane-segning
## ==========================================================

.PHONY: help init build up up-app start pull down destroy stop restart logs logs-app ps stats git-pull

init: 				## Initialize the project
	rm -rf packages/gen-server && docker compose -p emgr run --rm openapi-generator-cli $(c)

help:				## Show this help
	@sed -ne '/@sed/!s/## //p' $(MAKEFILE_LIST)

pull:				## Pull the image
	docker compose -p emgr -f compose.yaml pull $(c)

build: init			## Build the project
	docker compose -p emgr -f compose.yaml build $(c)
up: init 			## Start the project
	docker compose -p emgr -f compose.yaml up -d --remove-orphans --build $(c)
up-app: init			## Start app
	docker compose -p emgr -f compose.yaml up -d --remove-orphans --build app $(c)

start: init			## Start the project
	docker compose -p emgr -f compose.yaml start $(c)
down: 				## Stop the project
	docker compose -p emgr -f compose.yaml down $(c)
destroy: 			## Destroy the project
	docker compose -p emgr -f compose.yaml down -v $(c)
stop: 				## Stop the project
	docker compose -p emgr -f compose.yaml stop $(c)
restart: init			## Restart the project
	docker compose -p emgr -f compose.yaml stop $(c)
	docker compose -p emgr -f compose.yaml up -d $(c)

logs: 				## Show logs
	docker compose -p emgr -f compose.yaml logs --tail=100 -f $(c)
logs-app: 			## Show app logs
	docker compose -p emgr -f compose.yaml logs --tail=100 -f app $(c)
ps: 				## Show status
	docker compose -p emgr -f compose.yaml ps $(c)

stats: 				## Show stats
	docker compose -p emgr -f compose.yaml stats $(c)
	
git-pull:			## Git fetch link-frontend
	git submodule update --remote packages/link-frontend
