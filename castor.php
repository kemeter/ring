<?php

defined('CASTOR_USE_CHDIR') || define('CASTOR_USE_CHDIR', true);

use Castor\Attribute\AsTask;

use function Castor\context;
use function Castor\run;

#[AsTask(description: 'Start the Ring server')]
function start(): void
{
    $env = parse_ini_file(__DIR__ . '/.env.test');

    run(['cargo', 'run', '--', 'server', 'start'], context()->withEnvironment($env));
}
