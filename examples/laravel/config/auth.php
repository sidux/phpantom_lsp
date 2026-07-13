<?php

return [
    'defaults' => [
        'guard' => 'web',
    ],

    'guards' => [
        'web' => [
            'driver' => 'session',
            'provider' => 'users',
        ],
        'admin' => [
            'driver' => 'session',
            'provider' => 'admins',
        ],
    ],

    'providers' => [
        'users' => [
            'driver' => 'eloquent',
            'model' => App\Models\Customer::class,
        ],
        'admins' => [
            'driver' => 'eloquent',
            'model' => App\Models\Administrator::class,
        ],
    ],
];
